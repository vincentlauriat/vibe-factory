/// ASCII case-insensitive equality, the one comparison `find` and `search` share.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}
