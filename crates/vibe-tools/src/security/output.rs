//! Output hygiene: truncation of large tool output and binary detection.

/// Number of leading bytes inspected by [`is_probably_binary`] callers.
pub const BINARY_SNIFF_BYTES: usize = 8192;

/// Shorten `text` to at most `max_chars` characters of the original content,
/// keeping the beginning and the end and inserting a marker that says how many
/// characters were omitted in between.
///
/// Text that already fits is returned unchanged. Counting is done in Unicode
/// scalar values, so multi-byte characters are never split.
#[must_use]
pub fn truncate_output(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let head_len = max_chars / 2;
    let tail_len = max_chars - head_len;
    let omitted = total - head_len - tail_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total - tail_len).collect();
    format!("{head}\n\n[... {omitted} characters omitted ...]\n\n{tail}")
}

/// Heuristically decide whether `bytes` (typically the first
/// [`BINARY_SNIFF_BYTES`] of a file) come from a binary file.
///
/// A sample is considered binary when it contains a NUL byte, or when more
/// than 30% of its bytes are control characters other than common whitespace.
/// An empty sample is text.
#[must_use]
pub fn is_probably_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let suspicious = bytes
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | 0x0c | 0x1b | 0x08))
        .count();
    suspicious * 10 > bytes.len() * 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_unchanged() {
        assert_eq!(truncate_output("hello", 10), "hello");
        assert_eq!(truncate_output("", 0), "");
    }

    #[test]
    fn long_text_keeps_head_and_tail() {
        let text = format!("{}{}", "a".repeat(100), "z".repeat(100));
        let out = truncate_output(&text, 20);
        assert!(out.starts_with("aaaaaaaaaa\n"));
        assert!(out.ends_with("\nzzzzzzzzzz"));
        assert!(out.contains("180 characters omitted"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let text = "é".repeat(50);
        let out = truncate_output(&text, 10);
        assert!(out.starts_with("ééééé\n"));
        assert!(out.contains("40 characters omitted"));
    }

    #[test]
    fn binary_detection() {
        assert!(!is_probably_binary(b""));
        assert!(!is_probably_binary(
            b"fn main() {\n\tprintln!(\"hi\");\r\n}\n"
        ));
        assert!(!is_probably_binary("héllo wörld".as_bytes()));
        assert!(is_probably_binary(b"\x7fELF\x02\x01\x01\x00\x00"));
        assert!(is_probably_binary(&[1, 2, 3, 4, 5, 6, 7, b'a']));
    }
}
