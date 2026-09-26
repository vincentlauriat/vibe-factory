//! Conversation messages exchanged with a model.

/// Who authored a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The user (or the framework speaking on the user's behalf).
    User,
    /// The model.
    Assistant,
}

/// One block of a multi-part message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// Model reasoning (only present when the provider exposes it).
    Thinking {
        /// The reasoning text.
        text: String,
    },
    /// The model asks to call a tool.
    ToolUse {
        /// Provider-assigned call id, echoed back in the result.
        id: String,
        /// Tool name.
        name: String,
        /// Arguments matching the tool input schema.
        input: serde_json::Value,
    },
    /// The result of a tool call, sent back to the model.
    ToolResult {
        /// The call id this result answers.
        tool_use_id: String,
        /// Result content (text).
        content: String,
        /// Whether the tool failed.
        #[serde(default)]
        is_error: bool,
    },
}

/// A message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    /// Author.
    pub role: Role,
    /// Content blocks.
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// A user message with a single text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// An assistant message with a single text block.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// A user message carrying tool results.
    #[must_use]
    pub fn tool_results(results: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::User,
            content: results,
        }
    }

    /// Concatenated text of every [`ContentBlock::Text`] block.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every tool call requested in this message.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &serde_json::Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    /// Whether the message contains at least one tool call.
    #[must_use]
    pub fn has_tool_use(&self) -> bool {
        self.tool_uses().next().is_some()
    }

    /// Rough token estimate (4 characters per token), useful for context
    /// budgeting without a tokenizer.
    #[must_use]
    pub fn estimate_tokens(&self) -> usize {
        let chars: usize = self
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } | ContentBlock::Thinking { text } => text.len(),
                ContentBlock::ToolUse { input, name, .. } => name.len() + input.to_string().len(),
                ContentBlock::ToolResult { content, .. } => content.len(),
            })
            .sum();
        chars.div_ceil(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_tool_uses() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text { text: "hi".into() },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "a"}),
                },
            ],
        };
        assert_eq!(m.text(), "hi");
        assert!(m.has_tool_use());
        assert_eq!(m.tool_uses().count(), 1);
        assert!(m.estimate_tokens() > 0);
    }

    #[test]
    fn tagged_serde() {
        let b = ContentBlock::ToolResult {
            tool_use_id: "x".into(),
            content: "ok".into(),
            is_error: false,
        };
        let json = serde_json::to_value(&b).unwrap();
        assert_eq!(json["type"], "tool_result");
    }
}
