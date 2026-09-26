//! Classification of provider failures into [`ErrorKind`]s and secret
//! scrubbing for anything that ends up in logs or error messages.
//!
//! Every HTTP provider funnels non-success responses through
//! [`classify_http_error`] so that retry decisions and user-facing messages are
//! consistent regardless of the backend.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use vibe_core::{Error, ErrorKind};

/// Maximum number of characters of a response body kept in an error message.
const MAX_BODY_IN_MESSAGE: usize = 500;

/// Phrases that indicate an account/billing problem. Retrying cannot fix
/// these, so they win over every status-based rule (some APIs report an empty
/// balance with HTTP 429).
const BILLING_MARKERS: &[&str] = &[
    "billing",
    "credit balance",
    "insufficient credit",
    "out of credits",
    "insufficient_quota",
    "insufficient quota",
    "payment required",
];

/// Phrases that indicate rate limiting.
const RATE_LIMIT_MARKERS: &[&str] = &["rate limit", "rate_limit", "ratelimit", "too many requests"];

/// Phrases that indicate the prompt exceeds the model context window.
const CONTEXT_MARKERS: &[&str] = &[
    "context length",
    "context_length",
    "too long",
    "maximum context",
    "context window",
    "too many tokens",
];

/// Classify a non-success HTTP response into a framework [`Error`].
///
/// Rules, in priority order:
///
/// 1. billing / credit / insufficient quota wording (or HTTP 402) →
///    [`ErrorKind::InvalidRequest`] (not retryable);
/// 2. HTTP 429 or "rate limit" wording → [`ErrorKind::RateLimited`];
/// 3. HTTP 401 / 403 → [`ErrorKind::AuthFailed`];
/// 4. HTTP 400 / 413 mentioning the context length → [`ErrorKind::ContextTooLong`];
/// 5. any other 4xx → [`ErrorKind::InvalidRequest`] (408 is treated as
///    [`ErrorKind::Network`]);
/// 6. 5xx → [`ErrorKind::ServerError`].
///
/// The message contains the provider's own error text when the body is JSON,
/// otherwise a truncated copy of the body, always passed through
/// [`scrub_secrets`]. A `Retry-After` header is not visible here: callers
/// attach it with [`Error::with_retry_after`] (see [`parse_retry_after`]).
#[must_use]
pub fn classify_http_error(status: u16, body: &str) -> Error {
    let detail = error_detail(body);
    let lower = body.to_ascii_lowercase();
    let contains_any = |markers: &[&str]| markers.iter().any(|m| lower.contains(m));

    let kind = if status == 402 || contains_any(BILLING_MARKERS) {
        ErrorKind::InvalidRequest
    } else if status == 429 || contains_any(RATE_LIMIT_MARKERS) {
        ErrorKind::RateLimited
    } else if status == 401 || status == 403 {
        ErrorKind::AuthFailed
    } else if (status == 400 || status == 413) && contains_any(CONTEXT_MARKERS) {
        ErrorKind::ContextTooLong
    } else if status == 408 {
        ErrorKind::Network
    } else if (400..500).contains(&status) {
        ErrorKind::InvalidRequest
    } else if status >= 500 {
        ErrorKind::ServerError
    } else {
        ErrorKind::Other
    };

    let message = if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {detail}")
    };
    Error::new(kind, message)
}

/// Classify a transport-level failure (DNS, TLS, connect, timeout, broken
/// body stream) as [`ErrorKind::Network`], keeping the original error as the
/// source.
#[must_use]
pub fn classify_transport_error(err: reqwest::Error) -> Error {
    let what = if err.is_timeout() {
        "request timed out"
    } else if err.is_connect() {
        "connection failed"
    } else {
        "transport error"
    };
    let message = scrub_secrets(&format!("{what}: {err}"));
    Error::new(ErrorKind::Network, message).with_source(err)
}

/// Parse a `Retry-After` header value expressed in (possibly fractional)
/// seconds. HTTP-date values are ignored and yield `None`.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: f64 = value.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Extract a human readable error message from a response body.
fn error_detail(body: &str) -> String {
    let body = body.trim();
    let from_json = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let candidates = [
                v.pointer("/error/message"),
                v.pointer("/error"),
                v.pointer("/message"),
                v.pointer("/detail"),
            ];
            candidates
                .into_iter()
                .flatten()
                .find_map(|c| c.as_str().map(str::to_string))
        });
    let text = from_json.unwrap_or_else(|| body.to_string());
    truncate_chars(&scrub_secrets(&text), MAX_BODY_IN_MESSAGE)
}

/// Truncate to at most `max` characters, appending an ellipsis when cut.
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => format!("{}…", &text[..idx]),
        None => text.to_string(),
    }
}

static SECRET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // Provider API keys: sk-..., sk-ant-..., sk-proj-...
        (r"\bsk-[A-Za-z0-9_\-]{6,}", "sk-***"),
        // Authorization headers.
        (r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=\-]+", "${1}***"),
        // key=value and "key": "value" forms.
        (
            r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|x-api-key)(["']?\s*[=:]\s*["']?)[^\s&"',;]+"#,
            "${1}${2}***",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (
            Regex::new(pattern).expect("secret pattern is valid"),
            replacement,
        )
    })
    .collect()
});

/// Mask credentials in free text before it is logged or surfaced.
///
/// Handles `sk-…` keys, `Bearer …` tokens and `token=…` / `api_key=…`
/// assignments (including their JSON `"api_key": "…"` spelling).
#[must_use]
pub fn scrub_secrets(text: &str) -> String {
    SECRET_PATTERNS
        .iter()
        .fold(text.to_string(), |acc, (re, replacement)| {
            re.replace_all(&acc, *replacement).into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn billing_wins_over_rate_limit_status() {
        let body = r#"{"error":{"message":"You exceeded your current quota","code":"insufficient_quota"}}"#;
        let err = classify_http_error(429, body);
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
        assert!(!err.is_retryable());
        let err = classify_http_error(400, "Your credit balance is too low");
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
        assert_eq!(classify_http_error(402, "").kind, ErrorKind::InvalidRequest);
    }

    #[test]
    fn rate_limits() {
        assert_eq!(classify_http_error(429, "").kind, ErrorKind::RateLimited);
        assert_eq!(
            classify_http_error(400, "rate limit reached for requests").kind,
            ErrorKind::RateLimited
        );
    }

    #[test]
    fn auth_failures() {
        assert_eq!(classify_http_error(401, "nope").kind, ErrorKind::AuthFailed);
        assert_eq!(classify_http_error(403, "nope").kind, ErrorKind::AuthFailed);
    }

    #[test]
    fn context_too_long() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
        let err = classify_http_error(400, body);
        assert_eq!(err.kind, ErrorKind::ContextTooLong);
        assert!(err.message.contains("prompt is too long"));
        assert_eq!(
            classify_http_error(400, "This model's maximum context length is 128000 tokens").kind,
            ErrorKind::ContextTooLong
        );
    }

    #[test]
    fn other_client_and_server_errors() {
        assert_eq!(
            classify_http_error(400, "bad field").kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(
            classify_http_error(404, "no such model").kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(classify_http_error(408, "").kind, ErrorKind::Network);
        assert_eq!(classify_http_error(500, "").kind, ErrorKind::ServerError);
        assert_eq!(
            classify_http_error(529, "overloaded").kind,
            ErrorKind::ServerError
        );
        assert_eq!(classify_http_error(302, "").kind, ErrorKind::Other);
    }

    #[test]
    fn message_uses_json_error_text_and_is_scrubbed() {
        let body = r#"{"error":{"message":"invalid x-api-key sk-ant-abcdefghijkl"}}"#;
        let err = classify_http_error(401, body);
        assert_eq!(err.message, "HTTP 401: invalid x-api-key sk-***");
        assert_eq!(classify_http_error(500, "").message, "HTTP 500");
        let err = classify_http_error(500, r#"{"error":"boom"}"#);
        assert_eq!(err.message, "HTTP 500: boom");
    }

    #[test]
    fn long_bodies_are_truncated() {
        let body = "x".repeat(2000);
        let err = classify_http_error(500, &body);
        assert!(err.message.chars().count() < 600);
        assert!(err.message.ends_with('…'));
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(" 0.5 "), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn scrubbing() {
        assert_eq!(
            scrub_secrets("key sk-proj-ABCdef123456 used"),
            "key sk-*** used"
        );
        assert_eq!(
            scrub_secrets("Authorization: Bearer eyJhbGciOi.abc-def"),
            "Authorization: Bearer ***"
        );
        assert_eq!(
            scrub_secrets("GET /x?token=abc123&api_key=zzz&page=2"),
            "GET /x?token=***&api_key=***&page=2"
        );
        assert_eq!(
            scrub_secrets(r#"{"api_key": "secret-value"}"#),
            r#"{"api_key": "***"}"#
        );
        assert_eq!(scrub_secrets("nothing to hide"), "nothing to hide");
        assert_eq!(scrub_secrets("task-abcdef12"), "task-abcdef12");
    }
}
