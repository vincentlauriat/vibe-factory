use crate::money::format_cents;

/// Formats an invoice total given in cents, for example `Total: $1,234.50`.
pub fn invoice_line(total_cents: i64) -> String {
    format!("Total: {}", format_cents(total_cents))
}

#[cfg(test)]
mod tests {
    #[test]
    fn groups_thousands() {
        assert_eq!(super::invoice_line(123_450), "Total: $1,234.50");
    }
}
