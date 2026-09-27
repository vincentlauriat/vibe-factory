pub fn search<'a>(items: &[&'a str], query: &str) -> Vec<&'a str> {
    items
        .iter()
        .copied()
        .filter(|item| crate::compare::contains(item, query))
        .collect()
}
