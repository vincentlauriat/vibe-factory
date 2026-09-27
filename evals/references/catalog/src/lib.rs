mod compare;
mod lookup;
mod search;
pub use lookup::find;
pub use search::search;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_search() {
        let items = ["Alpha", "beta", "ALPHABET"];
        assert_eq!(find(&items, "alpha"), Some("Alpha"));
        assert_eq!(search(&items, "Alp"), vec!["Alpha", "ALPHABET"]);
        assert_eq!(search(&items, ""), items.to_vec());
        assert_eq!(find(&[], ""), None);
    }
}
