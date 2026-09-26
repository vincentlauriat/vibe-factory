//! Structured (JSON) output: extraction, repair and a retrying runner.
//!
//! Models are asked to end with a JSON document, but they sometimes wrap it in
//! prose, fence it, or emit slightly invalid JSON. The strategy is layered from
//! cheapest to most expensive:
//!
//! 1. [`extract_json`]: parse the whole text, else the last fenced block, else
//!    the last balanced top-level `{…}` / `[…]` span.
//! 2. [`repair_json`]: one cheap completion without tools asking the model to
//!    return only valid JSON.
//! 3. [`run_structured`]: re-run the agent once, continuing its transcript,
//!    with an explicit "your previous answer was not valid JSON" message.

use serde::de::DeserializeOwned;
use vibe_core::{
    AgentOutcome, AgentSpec, AgentStop, CompletionRequest, Error, ErrorKind, Message,
    ModelProvider, Result,
};

use crate::runtime::AgentRunner;

/// Extract a JSON object or array from free-form model output.
///
/// Tries, in order: the whole (trimmed) text, the last fenced code block
/// (a `json` fence is preferred over an untagged one), then the balanced
/// top-level `{…}` or `[…]` spans from last to first. Only objects and arrays
/// are accepted, so a stray `true` or `42` in prose is never returned.
#[must_use]
pub fn extract_json(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if let Some(v) = parse_container(trimmed) {
        return Some(v);
    }
    let fences = fenced_blocks(trimmed);
    let tagged = fences
        .iter()
        .rev()
        .filter(|(lang, _)| lang.eq_ignore_ascii_case("json"))
        .find_map(|(_, body)| parse_container(body));
    if tagged.is_some() {
        return tagged;
    }
    if let Some(v) = fences
        .iter()
        .rev()
        .filter(|(lang, _)| lang.is_empty())
        .find_map(|(_, body)| parse_container(body))
    {
        return Some(v);
    }
    balanced_spans(trimmed)
        .into_iter()
        .rev()
        .find_map(parse_container)
}

/// Extract JSON from `text` and deserialize it into `T`.
///
/// Fails with [`ErrorKind::InvalidRequest`] when no JSON is found or when it
/// does not match `T`.
pub fn parse_structured<T: DeserializeOwned>(text: &str) -> Result<T> {
    let value = extract_json(text).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidRequest,
            "no JSON object or array found in the output",
        )
    })?;
    from_value(value)
}

fn from_value<T: DeserializeOwned>(value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| {
        Error::new(
            ErrorKind::InvalidRequest,
            format!("JSON does not match the expected shape: {e}"),
        )
    })
}

fn parse_container(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    if !(s.starts_with('{') || s.starts_with('[')) {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .filter(|v| v.is_object() || v.is_array())
}

/// Every fenced code block as `(language, body)`, in order of appearance.
fn fenced_blocks(text: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut offset = 0;
    while let Some(open) = rest.find("```") {
        let after_ticks = &rest[open + 3..];
        let Some(line_end) = after_ticks.find('\n') else {
            break;
        };
        let lang = after_ticks[..line_end].trim().to_string();
        let body_start = open + 3 + line_end + 1;
        let body_rest = &rest[body_start..];
        let Some(close) = body_rest.find("```") else {
            break;
        };
        let abs_start = offset + body_start;
        out.push((lang, &text[abs_start..abs_start + close]));
        let consumed = body_start + close + 3;
        offset += consumed;
        rest = &rest[consumed..];
    }
    out
}

/// Top-level balanced `{…}` / `[…]` spans, skipping brackets inside strings.
fn balanced_spans(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == b'{' || bytes[i] == b'[')
            && let Some(end) = matching_end(bytes, i)
        {
            spans.push(&text[i..=end]);
            i = end + 1;
            continue;
        }
        i += 1;
    }
    spans
}

fn matching_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut stack: Vec<u8> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                if stack.pop() != Some(b) {
                    return None;
                }
                if stack.is_empty() {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// System prompt of the repair call.
const REPAIR_SYSTEM: &str = "You convert text into strictly valid JSON. \
Reply with the JSON document only: no prose, no markdown fences, no comments. \
Keep every piece of information from the input; do not invent values. \
Use double quotes, no trailing commas.";

/// Ask the model to turn `text` into valid JSON matching `schema_hint`.
///
/// This is a single completion without tools, at most 8192 output tokens.
pub async fn repair_json(
    provider: &dyn ModelProvider,
    model: &str,
    text: &str,
    schema_hint: &str,
) -> Result<serde_json::Value> {
    let prompt = format!(
        "The following output was supposed to be a JSON document with this shape:\n\n\
         {schema_hint}\n\n\
         Return only the corrected JSON document.\n\n\
         Output to repair:\n\n{text}"
    );
    let request = CompletionRequest {
        system: REPAIR_SYSTEM.to_string(),
        max_tokens: 8192,
        temperature: Some(0.0),
        ..CompletionRequest::new(model, vec![Message::user(prompt)])
    };
    let response = provider.complete(request).await?;
    extract_json(&response.message.text()).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidRequest,
            "the repair call did not return valid JSON",
        )
    })
}

/// Message sent to the agent when its answer could not be parsed.
#[must_use]
pub fn invalid_json_message(error: &str) -> String {
    format!("Your previous answer was not valid JSON: {error}. Return only JSON.")
}

/// Run `spec` and deserialize its final answer into `T`.
///
/// On failure, tries [`repair_json`] (using `T`'s type name and the start of
/// the system prompt as the hint), then resumes the agent once with
/// [`invalid_json_message`]. The returned outcome covers every agent run
/// (steps, tool calls, usage and transcript are accumulated).
pub async fn run_structured<T: DeserializeOwned>(
    runner: &AgentRunner,
    spec: &AgentSpec,
    message: String,
) -> Result<(T, AgentOutcome)> {
    run_structured_with_hint(runner, spec, message, std::any::type_name::<T>()).await
}

/// Like [`run_structured`] with an explicit schema hint for the repair call
/// (for example a JSON example of the expected document).
pub async fn run_structured_with_hint<T: DeserializeOwned>(
    runner: &AgentRunner,
    spec: &AgentSpec,
    message: String,
    schema_hint: &str,
) -> Result<(T, AgentOutcome)> {
    let first = runner.run(spec, message).await?;
    let error = match try_parse::<T>(runner, &first, schema_hint).await {
        Ok(value) => return Ok((value, first)),
        Err(e) => e,
    };
    // Retrying makes no sense when the run was cancelled or the provider failed.
    match &first.stop {
        AgentStop::Cancelled => {
            return Err(Error::new(ErrorKind::Cancelled, "agent run was cancelled"));
        }
        AgentStop::Error { kind, message } => {
            return Err(Error::new(
                *kind,
                format!("agent `{}` failed: {message}", spec.role),
            ));
        }
        _ => {}
    }
    tracing::debug!(role = %spec.role, %error, "structured output invalid, retrying agent");

    let second = runner
        .resume(
            spec,
            first.messages.clone(),
            invalid_json_message(&error.message),
        )
        .await?;
    let combined = merge_resumed(first, second);
    match parse_structured::<T>(&combined.final_text) {
        Ok(value) => Ok((value, combined)),
        Err(e) => Err(Error::new(
            ErrorKind::InvalidRequest,
            format!(
                "agent `{}` did not produce valid structured output after a retry: {}",
                spec.role, e.message
            ),
        )),
    }
}

async fn try_parse<T: DeserializeOwned>(
    runner: &AgentRunner,
    outcome: &AgentOutcome,
    schema_hint: &str,
) -> Result<T> {
    let error = match parse_structured::<T>(&outcome.final_text) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    if outcome.final_text.trim().is_empty() {
        return Err(error);
    }
    match repair_json(
        runner.provider().as_ref(),
        runner.model(),
        &outcome.final_text,
        schema_hint,
    )
    .await
    {
        Ok(value) => from_value(value),
        Err(repair_err) => {
            tracing::debug!(%repair_err, "JSON repair failed");
            Err(error)
        }
    }
}

/// Combine an outcome with the outcome of a [`AgentRunner::resume`] of its
/// transcript: the second transcript already contains the first one.
fn merge_resumed(first: AgentOutcome, second: AgentOutcome) -> AgentOutcome {
    AgentOutcome {
        steps: first.steps + second.steps,
        tool_calls: first.tool_calls + second.tool_calls,
        usage: first.usage.combined(second.usage),
        ..second
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn whole_text() {
        assert_eq!(extract_json(" {\"a\": 1} "), Some(json!({"a": 1})));
        assert_eq!(extract_json("[1,2]"), Some(json!([1, 2])));
    }

    #[test]
    fn fenced_json_prefers_last_json_fence() {
        let text = "Here:\n```json\n{\"a\": 1}\n```\nand finally\n```json\n{\"a\": 2}\n```\nDone.";
        assert_eq!(extract_json(text), Some(json!({"a": 2})));
    }

    #[test]
    fn json_fence_wins_over_untagged_fence() {
        let text = "```json\n{\"a\": 1}\n```\n```\n{\"b\": 2}\n```";
        assert_eq!(extract_json(text), Some(json!({"a": 1})));
    }

    #[test]
    fn untagged_fence_is_used() {
        let text = "Result:\n```\n{\"b\": 2}\n```";
        assert_eq!(extract_json(text), Some(json!({"b": 2})));
    }

    #[test]
    fn unfenced_balanced_span() {
        let text =
            "I checked {the files}. Final: {\"verdict\": \"approved\", \"note\": \"a } b\"} thanks";
        assert_eq!(
            extract_json(text),
            Some(json!({"verdict": "approved", "note": "a } b"}))
        );
    }

    #[test]
    fn nested_and_arrays() {
        let text = "prefix [ {\"x\": [1, {\"y\": 2}]} ] suffix";
        assert_eq!(extract_json(text), Some(json!([{"x": [1, {"y": 2}]}])));
    }

    #[test]
    fn rejects_scalars_and_garbage() {
        assert_eq!(extract_json("true"), None);
        assert_eq!(extract_json("42"), None);
        assert_eq!(extract_json("no json here"), None);
        assert_eq!(extract_json("{broken: json"), None);
    }

    #[test]
    fn parse_structured_into_type() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct S {
            status: String,
        }
        let s: S = parse_structured("ok\n```json\n{\"status\":\"done\"}\n```").unwrap();
        assert_eq!(s.status, "done");
        let err = parse_structured::<S>("{\"other\": 1}").unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
        assert!(parse_structured::<S>("nothing").is_err());
    }

    #[test]
    fn fenced_blocks_parses_languages() {
        let blocks = fenced_blocks("a\n```rust\nfn x() {}\n```\n```json\n{}\n```");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "rust");
        assert_eq!(blocks[1], ("json".to_string(), "{}\n"));
    }

    #[test]
    fn invalid_json_message_text() {
        assert_eq!(
            invalid_json_message("oops"),
            "Your previous answer was not valid JSON: oops. Return only JSON."
        );
    }
}
