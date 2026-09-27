/// Returned when a quoted field is not closed before the end of the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnterminatedQuote;

/// Splits one CSV record into its fields.
pub fn split_record(line: &str) -> Result<Vec<String>, UnterminatedQuote> {
    let line = line
        .strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line);
    let mut chars = line.chars().peekable();
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut at_field_start = true;
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == ',' {
            fields.push(std::mem::take(&mut field));
            at_field_start = true;
            continue;
        } else if c == '"' && at_field_start {
            in_quotes = true;
        } else {
            field.push(c);
        }
        at_field_start = false;
    }
    if in_quotes {
        return Err(UnterminatedQuote);
    }
    fields.push(field);
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_fields() {
        assert_eq!(split_record("a,b,c").unwrap(), ["a", "b", "c"]);
    }

    #[test]
    fn quoted_comma() {
        assert_eq!(split_record("\"x,y\",z").unwrap(), ["x,y", "z"]);
    }

    #[test]
    fn empty_fields_are_kept() {
        assert_eq!(split_record("").unwrap(), [""]);
        assert_eq!(split_record("a,").unwrap(), ["a", ""]);
    }

    #[test]
    fn terminator_and_literal_quote() {
        assert_eq!(split_record("ab\"c\r\n").unwrap(), ["ab\"c"]);
        assert_eq!(split_record("\"a"), Err(UnterminatedQuote));
    }
}
