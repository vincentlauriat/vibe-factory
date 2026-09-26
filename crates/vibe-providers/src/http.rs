//! Shared HTTP plumbing for the network-backed providers.
//!
//! All providers share one [`reqwest::Client`] (connection pooling, TLS
//! sessions) configured with sensible timeouts and a `vibe-factory/<version>`
//! user agent. Per-provider custom headers are applied per request rather than
//! baked into the client, so the pool stays shared.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use vibe_core::{Error, ErrorKind, Result};

use crate::classify::{
    classify_http_error, classify_transport_error, parse_retry_after, truncate_chars,
};

/// Timeout for establishing a TCP/TLS connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for a whole request, including reading the body. Long agentic
/// completions can take minutes.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// `User-Agent` sent with every request.
#[must_use]
pub fn user_agent() -> String {
    format!("vibe-factory/{}", env!("CARGO_PKG_VERSION"))
}

/// Build a new client with the framework defaults.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(user_agent())
        .build()
        .map_err(|e| Error::config(format!("cannot build HTTP client: {e}")).with_source(e))
}

/// The process-wide client shared by every provider.
///
/// # Panics
///
/// Panics if the TLS backend cannot be initialised, which only happens on a
/// broken system installation.
#[must_use]
pub fn shared_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| build_client().expect("default HTTP client configuration is valid"))
        .clone()
}

/// Read custom headers from a provider's `extra["headers"]` TOML table, e.g.
///
/// ```toml
/// [providers.gateway.extra.headers]
/// "X-Org" = "platform"
/// ```
///
/// Returns an empty map when the key is absent. Non-string values and invalid
/// header names or values are reported as [`ErrorKind::Config`].
pub fn headers_from_extra(extra: &BTreeMap<String, toml::Value>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let Some(value) = extra.get("headers") else {
        return Ok(headers);
    };
    let table = value
        .as_table()
        .ok_or_else(|| Error::config("provider `extra.headers` must be a table"))?;
    for (name, value) in table {
        let value = value
            .as_str()
            .ok_or_else(|| Error::config(format!("provider header `{name}` must be a string")))?;
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| Error::config(format!("invalid header name `{name}`: {e}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| Error::config(format!("invalid value for header `{name}`: {e}")))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

/// Normalise a base URL: trim whitespace and trailing slashes.
#[must_use]
pub fn normalize_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Send a prepared request and decode a JSON success body.
///
/// * transport failures become [`ErrorKind::Network`];
/// * non-2xx statuses go through [`classify_http_error`], with the
///   `Retry-After` header attached when present;
/// * a 2xx body that is not valid JSON becomes [`ErrorKind::ServerError`]
///   (the remote side misbehaved, not the caller).
pub async fn send_json(request: reqwest::RequestBuilder) -> Result<serde_json::Value> {
    let response = request.send().await.map_err(classify_transport_error)?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after);
    let body = response.text().await.map_err(classify_transport_error)?;

    if !status.is_success() {
        let mut err = classify_http_error(status.as_u16(), &body);
        if let Some(after) = retry_after {
            err = err.with_retry_after(after);
        }
        return Err(err);
    }

    serde_json::from_str(&body).map_err(|e| {
        Error::new(
            ErrorKind::ServerError,
            format!(
                "malformed JSON response ({e}): {}",
                truncate_chars(&crate::classify::scrub_secrets(&body), 200)
            ),
        )
        .with_source(e)
    })
}

/// Error returned when a provider has no credentials configured.
pub(crate) fn missing_key_error(provider: &str) -> Error {
    Error::new(
        ErrorKind::AuthFailed,
        format!("no API key configured for provider `{provider}` (set `api_key` or `api_key_env`)"),
    )
}

/// Error for a 2xx response whose JSON does not have the expected shape.
pub(crate) fn malformed(what: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::ServerError,
        format!("malformed provider response: {what}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_version() {
        assert!(user_agent().starts_with("vibe-factory/"));
        assert!(user_agent().len() > "vibe-factory/".len());
    }

    #[test]
    fn headers_parsing() {
        let extra: BTreeMap<String, toml::Value> =
            toml::from_str("[headers]\n\"X-Org\" = \"platform\"\nx-trace = \"1\"\n").unwrap();
        let h = headers_from_extra(&extra).unwrap();
        assert_eq!(h.get("x-org").unwrap(), "platform");
        assert_eq!(h.len(), 2);
        assert!(headers_from_extra(&BTreeMap::new()).unwrap().is_empty());
    }

    #[test]
    fn headers_errors() {
        let extra: BTreeMap<String, toml::Value> = toml::from_str("headers = 3\n").unwrap();
        assert_eq!(
            headers_from_extra(&extra).unwrap_err().kind,
            ErrorKind::Config
        );
        let extra: BTreeMap<String, toml::Value> = toml::from_str("[headers]\nx = 3\n").unwrap();
        assert_eq!(
            headers_from_extra(&extra).unwrap_err().kind,
            ErrorKind::Config
        );
        let extra: BTreeMap<String, toml::Value> =
            toml::from_str("[headers]\n\"bad header\" = \"v\"\n").unwrap();
        assert_eq!(
            headers_from_extra(&extra).unwrap_err().kind,
            ErrorKind::Config
        );
    }

    #[test]
    fn base_url_normalisation() {
        assert_eq!(normalize_base_url(" https://x.y/v1/ "), "https://x.y/v1");
        assert_eq!(normalize_base_url("http://a//"), "http://a");
    }

    #[test]
    fn shared_client_is_reusable() {
        let _a = shared_client();
        let _b = shared_client();
    }
}
