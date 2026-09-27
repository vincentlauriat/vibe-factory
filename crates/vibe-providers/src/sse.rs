//! Server-sent events, as used by the streaming APIs of the providers.
//!
//! [`SseParser`] turns a byte stream into events (`event:` and `data:`
//! fields, blank-line separated, `:` comments ignored). [`send_sse`] sends a
//! request, classifies an error status exactly like
//! [`crate::http::send_json`], then feeds every event to a callback.

use std::time::Duration;

use futures::StreamExt;
use vibe_core::{Error, ErrorKind, Result};

use crate::classify::{classify_http_error, classify_transport_error, parse_retry_after};

/// Longest a whole streamed response may take (the client default is shorter,
/// which suits one-shot requests only).
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(1800);

/// Longest silence between two chunks of a stream before it is abandoned.
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` field, if any.
    pub event: Option<String>,
    /// The `data:` lines, joined with `\n`.
    pub data: String,
}

/// Incremental parser of an event stream.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    /// Feed bytes; returns the events they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        // Line endings may be CRLF: carriage returns carry no meaning here.
        self.buffer
            .extend(bytes.iter().copied().filter(|b| *b != b'\r'));
        let mut events = Vec::new();
        while let Some(end) = self.buffer.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = self.buffer.drain(..end + 2).collect();
            if let Some(event) = parse_block(&String::from_utf8_lossy(&block)) {
                events.push(event);
            }
        }
        events
    }

    /// The event left without its terminating blank line, if any.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let rest = std::mem::take(&mut self.buffer);
        parse_block(&String::from_utf8_lossy(&rest))
    }
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = SseEvent::default();
    let mut data: Vec<&str> = Vec::new();
    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event.event = Some(value.to_string()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if data.is_empty() && event.event.is_none() {
        return None;
    }
    event.data = data.join("\n");
    Some(event)
}

/// What the callback of [`send_sse`] wants next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep reading.
    Continue,
    /// The response is complete; stop reading.
    Stop,
}

/// Send `request` and call `on_event` for every event of the response until
/// it returns [`Flow::Stop`] or the stream ends.
///
/// Errors before the first byte of a 2xx body are classified like
/// [`crate::http::send_json`] (so they may be retried); a stream that breaks
/// or stays silent for [`STREAM_IDLE_TIMEOUT`] afterwards is a
/// [`ErrorKind::Network`] error.
pub async fn send_sse(
    request: reqwest::RequestBuilder,
    mut on_event: impl FnMut(SseEvent) -> Result<Flow>,
) -> Result<()> {
    let response = request
        .timeout(STREAM_TIMEOUT)
        .send()
        .await
        .map_err(classify_transport_error)?;
    let status = response.status();
    if !status.is_success() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let body = response.text().await.map_err(classify_transport_error)?;
        let mut err = classify_http_error(status.as_u16(), &body);
        if let Some(after) = retry_after {
            err = err.with_retry_after(after);
        }
        return Err(err);
    }
    let mut parser = SseParser::default();
    let mut stream = response.bytes_stream();
    loop {
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Err(_) => {
                return Err(Error::new(
                    ErrorKind::Network,
                    format!(
                        "the stream stayed silent for {} seconds",
                        STREAM_IDLE_TIMEOUT.as_secs()
                    ),
                ));
            }
            Ok(None) => break,
            Ok(Some(chunk)) => chunk.map_err(classify_transport_error)?,
        };
        for event in parser.push(&chunk) {
            if on_event(event)? == Flow::Stop {
                return Ok(());
            }
        }
    }
    if let Some(event) = parser.finish() {
        on_event(event)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_across_chunks_and_line_endings() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: ping\r\ndata: {\"a\"").is_empty());
        let events = p.push(b":1}\r\n\r\n: comment\n\ndata: x\ndata: y\n\nevent: e\n");
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: Some("ping".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "x\ny".into()
                },
            ]
        );
        assert_eq!(
            p.finish(),
            Some(SseEvent {
                event: Some("e".into()),
                data: String::new()
            })
        );
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn utf8_split_between_chunks_is_kept() {
        let text = "data: héllo\n\n".as_bytes();
        let mut p = SseParser::default();
        assert!(p.push(&text[..8]).is_empty());
        assert_eq!(p.push(&text[8..])[0].data, "héllo");
    }
}
