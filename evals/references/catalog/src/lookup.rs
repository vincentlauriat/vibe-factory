pub fn find<'a>(items: &[&'a str], query: &str) -> Option<&'a str> {
    items.iter().copied().find(|item| crate::compare::same(item, query))
}
