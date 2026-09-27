//! Code forges: import issues as tasks, open pull requests for ready tasks.
//!
//! GitHub (and GitHub Enterprise) and GitLab (gitlab.com or self-managed)
//! are reached through their REST APIs with a token from the environment
//! (`GITHUB_TOKEN`/`GH_TOKEN`, `GITLAB_TOKEN`, or `token_env` in
//! `[integrations.<forge>]`). Public issues can be read without one.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use vibe_core::{ForgeConfig, VibeConfig};

/// A supported forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    /// GitHub.
    GitHub,
    /// GitLab.
    GitLab,
}

impl ForgeKind {
    /// Name used in configuration and task sources.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github",
            ForgeKind::GitLab => "gitlab",
        }
    }

    /// Parse `github` or `gitlab`.
    pub fn parse(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "github" => Ok(ForgeKind::GitHub),
            "gitlab" => Ok(ForgeKind::GitLab),
            other => bail!("unknown forge `{other}` (github or gitlab)"),
        }
    }
}

/// An issue reference: forge, repository path and number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    /// Forge.
    pub forge: ForgeKind,
    /// `owner/repo` (GitLab: `group/subgroup/project`).
    pub repo: String,
    /// Issue number (GitLab: iid).
    pub number: u64,
}

impl fmt::Display for IssueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

fn valid_repo(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

/// Parse `owner/repo#12`, `github:owner/repo#12`, `gitlab:group/project#5`,
/// or an issue URL of github.com or a GitLab instance. `default` is used for
/// the short form.
pub fn parse_issue_ref(text: &str, default: ForgeKind) -> Result<IssueRef> {
    let text = text.trim();
    let bad = || anyhow!("cannot read `{text}` as an issue: use owner/repo#12 or an issue URL");
    if let Some(rest) = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
    {
        let (host, path) = rest.split_once('/').ok_or_else(bad)?;
        let path = path.trim_end_matches('/');
        let (forge, repo, number) = if let Some((repo, n)) = path.split_once("/-/issues/") {
            (ForgeKind::GitLab, repo, n)
        } else if let Some((repo, n)) = path.rsplit_once("/issues/") {
            let forge = if host.contains("gitlab") {
                ForgeKind::GitLab
            } else {
                ForgeKind::GitHub
            };
            (forge, repo, n)
        } else {
            return Err(bad());
        };
        let number = number.parse().map_err(|_| bad())?;
        if !valid_repo(repo) {
            return Err(bad());
        }
        return Ok(IssueRef {
            forge,
            repo: repo.to_string(),
            number,
        });
    }
    let (forge, rest) = match text.split_once(':') {
        Some((name, rest)) => (ForgeKind::parse(name)?, rest),
        None => (default, text),
    };
    let (repo, number) = rest.rsplit_once('#').ok_or_else(bad)?;
    let number = number.parse().map_err(|_| bad())?;
    if !valid_repo(repo) {
        return Err(bad());
    }
    Ok(IssueRef {
        forge,
        repo: repo.to_string(),
        number,
    })
}

/// `owner/repo` of a git remote URL on `github.com`, `gitlab.com`, or any
/// host (`git@host:path.git`, `https://host/path.git`, `ssh://git@host/path`).
#[must_use]
pub fn repo_from_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url.strip_prefix("git@") {
        rest.split_once(':')?.1
    } else if let Some(pos) = url.find("://") {
        let after = &url[pos + 3..];
        after.split_once('/')?.1
    } else {
        return None;
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    valid_repo(path).then(|| path.to_string())
}

/// An issue read from a forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Title.
    pub title: String,
    /// Body (markdown).
    pub body: String,
    /// Labels.
    pub labels: Vec<String>,
    /// Web URL.
    pub url: String,
}

/// A client for one forge.
#[derive(Debug, Clone)]
pub struct Forge {
    kind: ForgeKind,
    api: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl Forge {
    /// Client for `kind`, configured from `[integrations.<kind>]`.
    #[must_use]
    pub fn from_config(kind: ForgeKind, config: &VibeConfig) -> Self {
        let settings = match kind {
            ForgeKind::GitHub => config.integrations.github.clone(),
            ForgeKind::GitLab => config.integrations.gitlab.clone(),
        }
        .unwrap_or_default();
        Self::new(kind, &settings)
    }

    /// Client for `kind` with explicit settings.
    #[must_use]
    pub fn new(kind: ForgeKind, settings: &ForgeConfig) -> Self {
        let api = settings
            .api_url
            .clone()
            .unwrap_or_else(|| {
                match kind {
                    ForgeKind::GitHub => "https://api.github.com",
                    ForgeKind::GitLab => "https://gitlab.com/api/v4",
                }
                .to_string()
            })
            .trim_end_matches('/')
            .to_string();
        let envs: Vec<String> = match &settings.token_env {
            Some(name) => vec![name.clone()],
            None => match kind {
                ForgeKind::GitHub => vec!["GITHUB_TOKEN".into(), "GH_TOKEN".into()],
                ForgeKind::GitLab => vec!["GITLAB_TOKEN".into()],
            },
        };
        let token = envs
            .iter()
            .find_map(|name| std::env::var(name).ok())
            .filter(|t| !t.trim().is_empty());
        Self {
            kind,
            api,
            token,
            client: reqwest::Client::builder()
                .user_agent(format!("vibe-factory/{}", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self.client.request(method, format!("{}{path}", self.api));
        request = match self.kind {
            ForgeKind::GitHub => request
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            ForgeKind::GitLab => request,
        };
        match (&self.token, self.kind) {
            (Some(t), ForgeKind::GitHub) => request.bearer_auth(t),
            (Some(t), ForgeKind::GitLab) => request.header("PRIVATE-TOKEN", t),
            (None, _) => request,
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder, what: &str) -> Result<Value> {
        let response = request
            .send()
            .await
            .with_context(|| format!("cannot reach {} to {what}", self.kind.name()))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let message = body["message"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| body.to_string());
            let hint = if self.token.is_none() && matches!(status.as_u16(), 401 | 403 | 404) {
                format!(
                    " (no token: set {})",
                    match self.kind {
                        ForgeKind::GitHub => "GITHUB_TOKEN",
                        ForgeKind::GitLab => "GITLAB_TOKEN",
                    }
                )
            } else {
                String::new()
            };
            bail!(
                "{} refused to {what}: HTTP {status}: {message}{hint}",
                self.kind.name()
            );
        }
        Ok(body)
    }

    fn project_path(repo: &str) -> String {
        repo.replace('/', "%2F")
    }

    /// Read an issue.
    pub async fn issue(&self, issue: &IssueRef) -> Result<Issue> {
        let path = match self.kind {
            ForgeKind::GitHub => format!("/repos/{}/issues/{}", issue.repo, issue.number),
            ForgeKind::GitLab => format!(
                "/projects/{}/issues/{}",
                Self::project_path(&issue.repo),
                issue.number
            ),
        };
        let body = self
            .send(
                self.request(reqwest::Method::GET, &path),
                &format!("read issue {issue}"),
            )
            .await?;
        if self.kind == ForgeKind::GitHub && body.get("pull_request").is_some() {
            bail!("{issue} is a pull request, not an issue");
        }
        let labels = body["labels"]
            .as_array()
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|l| l.as_str().or_else(|| l["name"].as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Issue {
            title: body["title"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string(),
            body: body[if self.kind == ForgeKind::GitHub {
                "body"
            } else {
                "description"
            }]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
            labels,
            url: body[if self.kind == ForgeKind::GitHub {
                "html_url"
            } else {
                "web_url"
            }]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        })
    }

    /// Open a pull request (GitHub) or merge request (GitLab) from `head`
    /// into `base`; returns its web URL.
    pub async fn open_pull_request(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<String> {
        if self.token.is_none() {
            bail!(
                "opening a pull request needs a token: set {}",
                match self.kind {
                    ForgeKind::GitHub => "GITHUB_TOKEN",
                    ForgeKind::GitLab => "GITLAB_TOKEN",
                }
            );
        }
        let (path, payload, url_key) = match self.kind {
            ForgeKind::GitHub => (
                format!("/repos/{repo}/pulls"),
                json!({"title": title, "head": head, "base": base, "body": body, "draft": draft}),
                "html_url",
            ),
            ForgeKind::GitLab => (
                format!("/projects/{}/merge_requests", Self::project_path(repo)),
                json!({
                    "title": if draft { format!("Draft: {title}") } else { title.to_string() },
                    "source_branch": head, "target_branch": base, "description": body,
                    "remove_source_branch": true
                }),
                "web_url",
            ),
        };
        let created = self
            .send(
                self.request(reqwest::Method::POST, &path).json(&payload),
                "open the pull request",
            )
            .await?;
        created[url_key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("the forge answered without a URL"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_references() {
        let r = parse_issue_ref("octo/app#12", ForgeKind::GitHub).unwrap();
        assert_eq!(
            (r.forge, r.repo.as_str(), r.number),
            (ForgeKind::GitHub, "octo/app", 12)
        );
        let r = parse_issue_ref("gitlab:group/sub/proj#5", ForgeKind::GitHub).unwrap();
        assert_eq!(
            (r.forge, r.repo.as_str(), r.number),
            (ForgeKind::GitLab, "group/sub/proj", 5)
        );
        let r =
            parse_issue_ref("https://github.com/octo/app/issues/7/", ForgeKind::GitLab).unwrap();
        assert_eq!((r.forge, r.number), (ForgeKind::GitHub, 7));
        let r = parse_issue_ref(
            "https://gitlab.example.com/g/p/-/issues/9",
            ForgeKind::GitHub,
        )
        .unwrap();
        assert_eq!(
            (r.forge, r.repo.as_str(), r.number),
            (ForgeKind::GitLab, "g/p", 9)
        );
        assert_eq!(r.to_string(), "g/p#9");
        for bad in [
            "octo/app",
            "app#1",
            "octo/app#x",
            "jira:A/B#1",
            "https://github.com/o/r/pull/3",
            "o/r?x#1",
        ] {
            assert!(parse_issue_ref(bad, ForgeKind::GitHub).is_err(), "{bad}");
        }
    }

    #[test]
    fn repositories_from_remotes() {
        assert_eq!(
            repo_from_remote("git@github.com:octo/app.git").as_deref(),
            Some("octo/app")
        );
        assert_eq!(
            repo_from_remote("https://github.com/octo/app").as_deref(),
            Some("octo/app")
        );
        assert_eq!(
            repo_from_remote("ssh://git@gitlab.com/g/sub/p.git").as_deref(),
            Some("g/sub/p")
        );
        assert_eq!(repo_from_remote("/srv/git/app.git"), None);
    }
}
