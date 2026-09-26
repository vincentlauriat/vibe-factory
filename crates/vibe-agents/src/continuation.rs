//! Continuation sessions: keep working after the context window fills up.
//!
//! When a run stops with [`AgentStop::ContextWindow`], its transcript is
//! summarised by a cheap tool-less completion and a fresh run starts from the
//! original request plus that summary. This repeats up to
//! [`ContinuationPolicy::max_continuations`] times.

use vibe_core::{
    AgentOutcome, AgentSpec, AgentStop, CompletionRequest, ContentBlock, Error, ErrorKind, Message,
    ModelProvider, Result, Role,
};

use crate::runtime::{AgentRunner, truncate_chars};

/// Maximum characters of one tool result kept in the transcript given to the
/// summariser.
const RESULT_EXCERPT_CHARS: usize = 1_500;
/// Maximum characters of one tool input kept in the transcript.
const INPUT_EXCERPT_CHARS: usize = 400;
/// Maximum characters of the whole rendered transcript (the most recent part
/// is kept).
const TRANSCRIPT_MAX_CHARS: usize = 300_000;

const SUMMARY_SYSTEM: &str = "You write handover notes for an AI software agent whose \
session ran out of context. The next session starts from scratch and only sees your notes. \
Be factual and specific: file paths, commands, decisions, what is done, what failed and why, \
and exactly what remains to do. Do not invent anything that is not in the transcript. \
Reply with the notes only.";

/// How many continuation sessions may follow a run that filled its context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationPolicy {
    /// Maximum number of continuation sessions (default 5).
    pub max_continuations: u32,
    /// Word budget of each summary (default 600).
    pub summary_max_words: usize,
}

impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self {
            max_continuations: 5,
            summary_max_words: 600,
        }
    }
}

impl ContinuationPolicy {
    /// Policy allowing `max_continuations` continuation sessions.
    #[must_use]
    pub fn new(max_continuations: u32) -> Self {
        Self {
            max_continuations,
            ..Self::default()
        }
    }
}

/// Render a transcript as plain text for summarisation. Long tool inputs and
/// results are shortened; if the whole text is too long, its oldest part is
/// dropped.
#[must_use]
pub fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        let who = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        for block in &m.content {
            match block {
                ContentBlock::Text { text } => {
                    out.push_str(&format!("[{who}] {}\n", text.trim()));
                }
                ContentBlock::Thinking { .. } => {}
                ContentBlock::ToolUse { name, input, .. } => {
                    let input = input.to_string();
                    out.push_str(&format!(
                        "[{who} called {name}] {}\n",
                        excerpt(&input, INPUT_EXCERPT_CHARS)
                    ));
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let tag = if *is_error {
                        "TOOL ERROR"
                    } else {
                        "TOOL RESULT"
                    };
                    out.push_str(&format!(
                        "[{tag}] {}\n",
                        excerpt(content.trim(), RESULT_EXCERPT_CHARS)
                    ));
                }
            }
        }
    }
    let total = out.chars().count();
    if total > TRANSCRIPT_MAX_CHARS {
        let skip = total - TRANSCRIPT_MAX_CHARS;
        let start = out.char_indices().nth(skip).map_or(0, |(i, _)| i);
        format!(
            "[… earlier part of the transcript omitted …]\n{}",
            &out[start..]
        )
    } else {
        out
    }
}

fn excerpt(s: &str, max: usize) -> String {
    let cut = truncate_chars(s, max);
    if cut.len() < s.len() {
        format!("{cut} […]")
    } else {
        cut.to_string()
    }
}

/// Summarise a transcript in at most about `max_words` words with one
/// completion and no tools.
pub async fn summarize_transcript(
    provider: &dyn ModelProvider,
    model: &str,
    messages: &[Message],
    max_words: usize,
) -> Result<String> {
    let transcript = render_transcript(messages);
    let prompt = format!(
        "Summarise this agent session in at most {max_words} words, as handover notes with \
         these sections: Goal, Done so far, Key findings (files, commands, decisions), \
         Problems, Next steps.\n\n--- TRANSCRIPT ---\n{transcript}--- END ---"
    );
    let max_tokens = u32::try_from(max_words.saturating_mul(2).saturating_add(256))
        .unwrap_or(u32::MAX)
        .min(16_384);
    let request = CompletionRequest {
        system: SUMMARY_SYSTEM.to_string(),
        max_tokens,
        ..CompletionRequest::new(model, vec![Message::user(prompt)])
    };
    let response = provider.complete(request).await?;
    let summary = response.message.text().trim().to_string();
    if summary.is_empty() {
        return Err(Error::new(
            ErrorKind::Other,
            "the summariser returned an empty summary",
        ));
    }
    Ok(summary)
}

/// Message that starts a continuation session.
#[must_use]
pub fn continuation_message(original: &str, summary: &str) -> String {
    format!(
        "{original}\n\nSummary of your previous session:\n{summary}\n\nContinue where you left off."
    )
}

/// Run `spec`; while a run stops with [`AgentStop::ContextWindow`], summarise
/// it and start a fresh run from the original message plus the summary, up
/// to `policy.max_continuations` times.
///
/// The returned outcome concatenates every session: transcripts are appended,
/// steps, tool calls and usage are summed, and the stop reason and final text
/// are those of the last session. If summarisation fails, the last outcome is
/// returned as is.
pub async fn run_with_continuation(
    runner: &AgentRunner,
    spec: &AgentSpec,
    message: String,
    policy: ContinuationPolicy,
) -> Result<AgentOutcome> {
    let mut combined = runner.run(spec, message.clone()).await?;
    let mut last_messages = combined.messages.clone();
    let mut continuations = 0;
    while combined.stop == AgentStop::ContextWindow && continuations < policy.max_continuations {
        let summary = match summarize_transcript(
            runner.provider().as_ref(),
            runner.model(),
            &last_messages,
            policy.summary_max_words,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "continuation summary failed");
                break;
            }
        };
        continuations += 1;
        runner
            .events()
            .log(
                Some(runner.current_run_id()),
                "info",
                format!(
                    "agent `{}` hit the context window; continuation {continuations}/{}",
                    spec.role, policy.max_continuations
                ),
            )
            .await;
        let next = runner
            .run(spec, continuation_message(&message, &summary))
            .await?;
        last_messages = next.messages.clone();
        combined = concat_outcomes(combined, next);
    }
    Ok(combined)
}

/// Concatenate two independent sessions.
fn concat_outcomes(first: AgentOutcome, second: AgentOutcome) -> AgentOutcome {
    let mut messages = first.messages;
    messages.extend(second.messages);
    AgentOutcome {
        role: second.role,
        stop: second.stop,
        final_text: second.final_text,
        messages,
        steps: first.steps + second.steps,
        tool_calls: first.tool_calls + second.tool_calls,
        usage: first.usage.combined(second.usage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transcript_rendering_shortens_and_skips_thinking() {
        let messages = vec![
            Message::user("do it"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        text: "secret".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "1".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a.rs"}),
                    },
                ],
            },
            Message::tool_results(vec![ContentBlock::ToolResult {
                tool_use_id: "1".into(),
                content: "y".repeat(5_000),
                is_error: false,
            }]),
        ];
        let t = render_transcript(&messages);
        assert!(t.contains("[USER] do it"));
        assert!(t.contains("[ASSISTANT called read_file]"));
        assert!(!t.contains("secret"));
        assert!(t.contains("[…]"));
        assert!(t.len() < 2_000);
    }

    #[test]
    fn transcript_keeps_most_recent_part() {
        let messages: Vec<Message> = (0..400)
            .map(|i| Message::user(format!("{i}:{}", "z".repeat(1_000))))
            .collect();
        let t = render_transcript(&messages);
        assert!(t.starts_with("[… earlier part"));
        assert!(t.contains("399:"));
        assert!(!t.contains("[USER] 0:"));
    }

    #[test]
    fn default_policy() {
        assert_eq!(ContinuationPolicy::default().max_continuations, 5);
        assert_eq!(ContinuationPolicy::new(2).max_continuations, 2);
    }

    #[test]
    fn continuation_message_format() {
        let m = continuation_message("Task X", "did A");
        assert!(m.starts_with("Task X\n\nSummary of your previous session:\ndid A"));
        assert!(m.ends_with("Continue where you left off."));
    }
}
