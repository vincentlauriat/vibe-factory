//! Minimal `{{variable}}` templating for prompts.

use std::collections::BTreeMap;

/// A prompt template with `{{name}}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    source: String,
}

impl PromptTemplate {
    /// Wrap a template string.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
        }
    }

    /// Raw template text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Placeholder names, in order of first appearance.
    #[must_use]
    pub fn placeholders(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = self.source.as_str();
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else { break };
            let name = after[..end].trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            rest = &after[end + 2..];
        }
        out
    }

    /// Substitute placeholders. Unknown placeholders are replaced by an
    /// empty string so that a prompt never leaks raw `{{...}}` markers.
    #[must_use]
    pub fn render(&self, vars: &BTreeMap<String, String>) -> String {
        let mut out = String::with_capacity(self.source.len());
        let mut rest = self.source.as_str();
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find("}}") {
                Some(end) => {
                    let name = after[..end].trim();
                    if let Some(v) = vars.get(name) {
                        out.push_str(v);
                    }
                    rest = &after[end + 2..];
                }
                None => {
                    out.push_str("{{");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Convenience: render with `(key, value)` pairs.
    #[must_use]
    pub fn render_with<'a>(&self, pairs: impl IntoIterator<Item = (&'a str, String)>) -> String {
        let vars = pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        self.render(&vars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_and_blanks_unknown() {
        let t = PromptTemplate::new("Hello {{ name }}, task: {{task}}. {{missing}}!");
        assert_eq!(t.placeholders(), vec!["name", "task", "missing"]);
        let out = t.render_with([("name", "V".to_string()), ("task", "x".to_string())]);
        assert_eq!(out, "Hello V, task: x. !");
    }

    #[test]
    fn unterminated_marker_is_kept() {
        let t = PromptTemplate::new("a {{ b");
        assert_eq!(t.render(&BTreeMap::new()), "a {{ b");
    }
}
