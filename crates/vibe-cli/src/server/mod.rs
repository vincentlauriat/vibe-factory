//! `vibe serve`: the run manager over HTTP, with the web UI.
//!
//! The server binds to the loopback interface by default, accepts only its
//! own `Host` names there (against DNS rebinding) and requires a bearer
//! token on every API call. The token is random per start, printed once and
//! written to `.vibe/server.token` (readable by the owner only on Unix). It
//! never returns configuration or secrets.

pub mod api;

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::json;
use vibe_core::config::VIBE_DIR;
use vibe_pipeline::RunManager;

use crate::app::{Overrides, build_context};
use crate::cli::ServeArgs;
use crate::util::{Ui, style};

/// File holding the token of the running server.
pub const TOKEN_FILE: &str = "server.token";

/// Longest wait for runs to stop when the server shuts down.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// A random 256-bit token, hex encoded.
fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn write_token(root: &Path, token: &str) -> Result<()> {
    let path = root.join(VIBE_DIR).join(TOKEN_FILE);
    std::fs::create_dir_all(root.join(VIBE_DIR))?;
    std::fs::write(&path, format!("{token}\n"))
        .with_context(|| format!("cannot write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Serve until `Ctrl-C`; runs started by the server are then cancelled
/// (they stay resumable).
pub async fn run(root: &Path, args: ServeArgs, ui: Ui) -> Result<u8> {
    let overrides = Overrides {
        provider: args.provider.clone(),
        model: args.model.clone(),
        workspace: args.workspace.clone(),
        auto_merge: false,
        script: args.script.clone(),
        max_tokens: None,
        max_duration_secs: None,
    };
    let ctx = Arc::new(build_context(root, &overrides).await?);
    let manager = RunManager::new(ctx.pipeline());
    let token = new_token();
    write_token(root, &token)?;

    let ip: IpAddr = args
        .bind
        .parse()
        .with_context(|| format!("invalid address `{}`", args.bind))?;
    let listener = tokio::net::TcpListener::bind((ip, args.port))
        .await
        .with_context(|| format!("cannot listen on {}:{}", args.bind, args.port))?;
    let port = listener.local_addr()?.port();
    let allowed_hosts = if ip.is_loopback() {
        vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]
    } else {
        eprintln!(
            "{} listening on {ip}: anyone who can reach this address and has the token controls \
             the agents",
            style::warn().apply_to("!")
        );
        Vec::new()
    };
    let host = if ip.is_loopback() {
        "127.0.0.1".to_string()
    } else {
        ip.to_string()
    };
    let url = format!("http://{host}:{port}/");
    if ui.json {
        ui.print_json(&json!({"url": url, "port": port, "token": token}));
    } else {
        println!("vibe serve on {url}");
        println!("open {url}#token={token}");
        println!("API token (also in .vibe/{TOKEN_FILE}): {token}");
        println!("Ctrl-C to stop");
    }
    use std::io::Write;
    std::io::stdout().flush()?;

    let state = Arc::new(api::Inner {
        ctx: Arc::clone(&ctx),
        manager: manager.clone(),
        token,
        allowed_hosts,
    });
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    for task in manager.active() {
        manager.cancel(task);
    }
    let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
    while !manager.active().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = std::fs::remove_file(root.join(VIBE_DIR).join(TOKEN_FILE));
    if let Ok(ctx) = Arc::try_unwrap(ctx) {
        ctx.shutdown().await;
    }
    Ok(0)
}
