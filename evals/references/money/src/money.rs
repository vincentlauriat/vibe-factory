/// Formats an amount in cents as `$1,234.56`, with a leading `-` when negative.
pub fn format_cents(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let magnitude = cents.unsigned_abs();
    let digits = (magnitude / 100).to_string();
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{sign}${grouped}.{:02}", magnitude % 100)
}

#[cfg(test)]
mod tests {
    use super::format_cents;

    #[test]
    fn formats_edges() {
        assert_eq!(format_cents(0), "$0.00");
        assert_eq!(format_cents(5), "$0.05");
        assert_eq!(format_cents(123_456), "$1,234.56");
        assert_eq!(format_cents(-100_000), "-$1,000.00");
        assert_eq!(format_cents(i64::MIN), "-$92,233,720,368,547,758.08");
    }
}
