//! Text helpers shared by the tools: line numbering and unified diffs.

use similar::TextDiff;

/// Maximum number of characters shown for a single line by `read_file`.
pub const MAX_LINE_CHARS: usize = 2000;

/// Shorten `line` to at most `max_chars` characters, appending a note with the
/// original length when something was cut.
#[must_use]
pub fn truncate_line(line: &str, max_chars: usize) -> String {
    let total = line.chars().count();
    if total <= max_chars {
        return line.to_string();
    }
    let kept: String = line.chars().take(max_chars).collect();
    format!("{kept}… [line truncated, {total} characters in total]")
}

/// Format one line the way `cat -n` does: the 1-based number right-aligned
/// on six columns, a tab, then the line content.
#[must_use]
pub fn number_line(number: usize, line: &str) -> String {
    format!("{number:>6}\t{line}")
}

/// Number every line of `lines`, starting at `first_number`, truncating each
/// line at `max_line_chars` characters. Lines are joined with `\n`.
#[must_use]
pub fn number_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    first_number: usize,
    max_line_chars: usize,
) -> String {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| number_line(first_number + i, &truncate_line(l, max_line_chars)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a unified diff (three lines of context) between `old` and `new`,
/// labelled `a/<path>` and `b/<path>`. Returns an empty string when the two
/// texts are identical.
#[must_use]
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    if old == new {
        return String::new();
    }
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn numbering_matches_cat_n() {
        assert_eq!(number_line(7, "x"), "     7\tx");
        assert_eq!(number_lines(["a", "b"], 9, 100), "     9\ta\n    10\tb");
    }

    #[test]
    fn long_lines_are_truncated() {
        let out = truncate_line(&"x".repeat(10), 4);
        assert_eq!(out, "xxxx… [line truncated, 10 characters in total]");
        assert_eq!(truncate_line("abc", 4), "abc");
    }

    #[test]
    fn diff_has_headers_and_hunks() {
        let d = unified_diff("a\nb\nc\n", "a\nB\nc\n", "src/x.rs");
        assert!(d.contains("--- a/src/x.rs"));
        assert!(d.contains("+++ b/src/x.rs"));
        assert!(d.contains("-b\n"));
        assert!(d.contains("+B\n"));
        assert_eq!(unified_diff("same", "same", "p"), "");
    }
}
