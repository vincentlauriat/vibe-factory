pub fn search<'a>(items: &[&'a str], query: &str) -> Vec<&'a str> {
    items.iter().copied().filter(|item| contains(item, query)).collect()
}

/// An empty query matches every item.
fn contains(item: &str, query: &str) -> bool {
    let query = query.as_bytes();
    query.is_empty()
        || item
            .as_bytes()
            .windows(query.len())
            .any(|window| crate::compare::same(window, query))
}
