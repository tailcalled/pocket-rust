// Sibling module to `mod.rs` inside the same directory: a `mod.rs`
// anchors its own directory, so `mod inner;` declared inside
// `sub/mod.rs` looks for `sub/inner.rs` (not `sub/mod/inner.rs`).
pub fn compute() -> u32 {
    42u32
}
