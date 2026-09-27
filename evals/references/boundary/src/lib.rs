pub fn contains(value: i32, min: i32, max: i32) -> bool {
    min <= max && value >= min && value <= max
}

#[cfg(test)]
mod tests {
    use super::contains;

    #[test]
    fn bounds_are_inclusive() {
        assert!(contains(10, 0, 10));
        assert!(contains(0, 0, 10));
        assert!(!contains(5, 9, 2));
    }
}
