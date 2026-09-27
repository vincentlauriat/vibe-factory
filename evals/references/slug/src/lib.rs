pub fn greeting() -> &'static str { "hello" }

pub fn slug(input: &str) -> String {
    let mut out = String::new();
    let mut pending_hyphen = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_hyphen = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slugs() {
        assert_eq!(slug("  Hello, WORLD!  "), "hello-world");
        assert_eq!(slug("aéb"), "a-b");
        assert_eq!(slug(""), "");
    }
}
