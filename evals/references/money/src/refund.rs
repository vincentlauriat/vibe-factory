use crate::money::format_cents;

/// Formats a refund amount given in cents, for example `Refund: $12.50`.
pub fn refund_line(amount_cents: i64) -> String {
    format!("Refund: {}", format_cents(amount_cents))
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_dollars_and_cents() {
        assert_eq!(super::refund_line(1250), "Refund: $12.50");
        assert_eq!(super::refund_line(1205), "Refund: $12.05");
    }
}
