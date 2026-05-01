// Binary entry point for the language-intrinsics test suite. The
// actual helpers and submodule declarations live in
// `tests/lang/mod.rs`; mod-file path resolution picks up
// `tests/lang/<feature>.rs` automatically from there.
#[path = "lang/mod.rs"]
mod lang;
