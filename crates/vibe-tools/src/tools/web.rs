//! `web_fetch` and `web_search`: read the web, behind the network permission.
//!
//! Both refuse to run unless the agent was granted network access
//! (`security.allow_network`). `web_fetch` only reaches public addresses:
//! host names resolving to loopback, private, link-local or otherwise
//! internal addresses are refused, and redirects are followed by hand so that
//! every hop is checked (against server-side request forgery). An optional
//! domain allowlist narrows it further. HTML is turned into plain text.
//! `web_search` queries a SearXNG-compatible endpoint the project configures.

use std::net::IpAddr;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use vibe_core::{Result, Tool, ToolContext, ToolOutput};

use super::common::{MAX_OUTPUT_CHARS, parse_input, permission_denied, try_output};

/// Longest a request may take.
pub const WEB_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest body read, in bytes.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Redirects followed at most.
pub const MAX_REDIRECTS: usize = 5;
/// Results returned by `web_search` at most.
pub const MAX_SEARCH_RESULTS: usize = 10;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(WEB_TIMEOUT)
        .user_agent(format!("vibe-factory/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Whether `ip` is an address a fetch must never reach from an agent.
#[must_use]
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
                // Carrier-grade NAT, 100.64.0.0/10.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique local fc00::/7 and link-local fe80::/10.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| is_internal(IpAddr::V4(v4)))
        }
    }
}

/// Whether `host` is `domain` or one of its subdomains.
fn domain_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim().trim_start_matches("*.").to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// Fetches a web page as text.
#[derive(Debug, Clone)]
pub struct WebFetchTool {
    allowed_domains: Vec<String>,
    allow_internal: bool,
    client: reqwest::Client,
}

impl WebFetchTool {
    /// Tool limited to `allowed_domains` (and their subdomains); empty means
    /// any public host.
    #[must_use]
    pub fn new(allowed_domains: Vec<String>) -> Self {
        Self {
            allowed_domains,
            allow_internal: false,
            client: client(),
        }
    }

    /// Allow loopback and private addresses. For tests against a local
    /// server only.
    #[must_use]
    pub fn allow_internal_addresses(mut self) -> Self {
        self.allow_internal = true;
        self
    }

    async fn check_url(&self, url: &reqwest::Url) -> std::result::Result<(), String> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!(
                "only http and https URLs can be fetched, not `{}`",
                url.scheme()
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| "the URL has no host".to_string())?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if !self.allowed_domains.is_empty()
            && !self.allowed_domains.iter().any(|d| domain_matches(bare, d))
        {
            return Err(format!(
                "`{host}` is not in the allowed domains ({})",
                self.allowed_domains.join(", ")
            ));
        }
        if self.allow_internal {
            return Ok(());
        }
        let port = url.port_or_known_default().unwrap_or(80);
        let addresses: Vec<IpAddr> = match bare.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => tokio::net::lookup_host((bare, port))
                .await
                .map_err(|e| format!("cannot resolve `{host}`: {e}"))?
                .map(|a| a.ip())
                .collect(),
        };
        if addresses.is_empty() || addresses.iter().any(|ip| is_internal(*ip)) {
            return Err(format!(
                "`{host}` resolves to an internal address; only public hosts can be fetched"
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchInput {
    url: String,
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a public web page or document over http(s) and return its text (HTML is \
         converted to plain text). Use it to read documentation, changelogs or issues. Only \
         available when network access is granted; internal and private addresses are \
         refused. At most 30000 characters are returned."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Absolute http(s) URL."}
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.network {
            return Ok(permission_denied("reach the network"));
        }
        let input: FetchInput = try_output!(parse_input(self.name(), input));
        let mut url = match reqwest::Url::parse(input.url.trim()) {
            Ok(u) => u,
            Err(e) => return Ok(ToolOutput::error(format!("Invalid URL: {e}."))),
        };
        let mut hops = 0;
        let response = loop {
            if let Err(why) = self.check_url(&url).await {
                return Ok(ToolOutput::error(format!("Refused: {why}.")));
            }
            let response = match self.client.get(url.clone()).send().await {
                Ok(r) => r,
                Err(e) => return Ok(ToolOutput::error(format!("Request failed: {e}."))),
            };
            if !response.status().is_redirection() {
                break response;
            }
            hops += 1;
            let Some(next) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .and_then(|l| url.join(l).ok())
            else {
                return Ok(ToolOutput::error("Redirect without a valid location."));
            };
            if hops > MAX_REDIRECTS {
                return Ok(ToolOutput::error("Too many redirects."));
            }
            url = next;
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = match read_limited(response).await {
            Ok(b) => b,
            Err(e) => return Ok(ToolOutput::error(format!("Cannot read the response: {e}."))),
        };
        let textual = content_type.is_empty()
            || content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
            || content_type.contains("javascript");
        if !textual || crate::is_probably_binary(&body) {
            return Ok(ToolOutput::error(format!(
                "`{url}` returned binary content ({content_type}); only text can be read."
            )));
        }
        let raw = String::from_utf8_lossy(&body);
        let text = if content_type.contains("html") {
            html_to_text(&raw)
        } else {
            raw.into_owned()
        };
        let text = crate::truncate_output(&text, MAX_OUTPUT_CHARS);
        let mut out = if status.is_success() {
            ToolOutput::ok(text)
        } else {
            ToolOutput::error(format!("HTTP {status} from {url}\n\n{text}"))
        };
        out.metadata = json!({
            "status": status.as_u16(),
            "url": url.as_str(),
            "content_type": content_type,
        });
        Ok(out)
    }
}

async fn read_limited(mut response: reqwest::Response) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = MAX_BODY_BYTES.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= MAX_BODY_BYTES {
            break;
        }
    }
    Ok(body)
}

/// Plain text of an HTML document: scripts, styles and tags removed, block
/// elements on their own lines, common entities decoded, blank runs folded.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Skip the content of script, style and similar elements.
            let skipped = ["script", "style", "noscript", "svg", "head"]
                .iter()
                .find(|t| lower[i + 1..].starts_with(*t));
            if let Some(tag) = skipped {
                let close = format!("</{tag}");
                let end = lower[i..].find(&close).map_or(bytes.len(), |p| i + p);
                i = lower[end..].find('>').map_or(bytes.len(), |p| end + p + 1);
                continue;
            }
            let end = lower[i..].find('>').map_or(bytes.len(), |p| i + p + 1);
            let tag = &lower[i..end];
            let name = tag
                .trim_start_matches('<')
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or("");
            let closing_item = name == "li" && tag.starts_with("</");
            if !closing_item
                && matches!(
                    name,
                    "p" | "div"
                        | "br"
                        | "li"
                        | "tr"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "pre"
                        | "section"
                        | "article"
                        | "header"
                        | "footer"
                        | "ul"
                        | "ol"
                        | "table"
                        | "blockquote"
                )
            {
                out.push('\n');
            }
            if name == "li" && !tag.starts_with("</") {
                out.push_str("- ");
            }
            i = end;
            continue;
        }
        let next = html[i..].find('<').map_or(html.len(), |p| i + p);
        out.push_str(&decode_entities(&html[i..next]));
        i = next;
    }
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank == 1 && !text.is_empty() {
                text.push('\n');
            }
            continue;
        }
        blank = 0;
        text.push_str(&line);
        text.push('\n');
    }
    text.trim().to_string()
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Searches the web through a SearXNG-compatible JSON endpoint.
#[derive(Debug, Clone)]
pub struct WebSearchTool {
    endpoint: String,
    client: reqwest::Client,
}

impl WebSearchTool {
    /// Tool querying `endpoint`, a URL with a `{query}` placeholder that
    /// answers SearXNG JSON (`{"results": [{"title", "url", "content"}]}`),
    /// such as `https://search.example/search?q={query}&format=json`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: client(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

fn encode_query(query: &str) -> String {
    let mut out = String::new();
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web and return the first results (title, URL and snippet). Follow up \
         with `web_fetch` to read a page. Only available when network access is granted."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for."}
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.network {
            return Ok(permission_denied("reach the network"));
        }
        let input: SearchInput = try_output!(parse_input(self.name(), input));
        if input.query.trim().is_empty() {
            return Ok(ToolOutput::error("The query is empty."));
        }
        let url = self
            .endpoint
            .replace("{query}", &encode_query(input.query.trim()));
        let response = match self.client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                return Ok(ToolOutput::error(format!(
                    "Search failed: HTTP {}.",
                    r.status()
                )));
            }
            Err(e) => return Ok(ToolOutput::error(format!("Search failed: {e}."))),
        };
        let parsed: SearchResponse = match response.json().await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "The search endpoint did not answer SearXNG JSON: {e}."
                )));
            }
        };
        if parsed.results.is_empty() {
            return Ok(ToolOutput::ok("No result."));
        }
        let text = parsed
            .results
            .iter()
            .take(MAX_SEARCH_RESULTS)
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "{}. {}\n   {}\n   {}",
                    i + 1,
                    r.title.trim(),
                    r.url.trim(),
                    r.content.split_whitespace().collect::<Vec<_>>().join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(crate::truncate_output(
            &text,
            MAX_OUTPUT_CHARS,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_internal(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "2606:4700::1111", "100.128.0.1"] {
            assert!(!is_internal(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn domains() {
        assert!(domain_matches("docs.rs", "docs.rs"));
        assert!(domain_matches("api.docs.rs", "*.docs.rs"));
        assert!(!domain_matches("evildocs.rs", "docs.rs"));
        assert!(!domain_matches("docs.rs", ""));
    }

    #[test]
    fn html_becomes_text() {
        let html = "<html><head><title>x</title><style>p{}</style></head><body>\
                    <h1>Title &amp; more</h1><script>alert(1)</script><p>First&nbsp;para</p>\
                    <ul><li>one</li><li>two &#x2713;</li></ul><p>a &lt;b&gt; &#39;c&#39; &bogus;</p></body></html>";
        assert_eq!(
            html_to_text(html),
            "Title & more\n\nFirst para\n\n- one\n- two ✓\n\na <b> 'c' &bogus;"
        );
    }

    #[test]
    fn queries_are_encoded() {
        assert_eq!(
            encode_query("rust async/await é"),
            "rust+async%2Fawait+%C3%A9"
        );
    }
}
