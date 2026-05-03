// Binary entry point for the **gaps** test suite — known soundness
// holes, partially-implemented features, and behaviors pocket-rust
// gets wrong today. Tests in here are NOT `#[ignore]`'d and NOT
// inverted: they fail honestly. CI failures here are expected and
// document outstanding work. When a gap gets fixed, the test starts
// passing — promote it to the appropriate suite (`tests/lang/`,
// `tests/std/`, etc.) and rewrite it as a regular positive/negative
// test there.
//
// See `tests/gaps/mod.rs` for the test inventory and naming
// conventions.
#[path = "gaps/mod.rs"]
mod gaps;
