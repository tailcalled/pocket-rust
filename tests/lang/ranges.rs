// Range literal expressions — `a..b`, `a..`, `..b`, `..`, `a..=b`,
// `..=b`. Parse-time desugar to `std::ops::Range*` struct literals,
// so the rest of the pipeline (typeck, codegen) sees plain struct
// construction. Slicing impls (Index<Range<usize>> etc.) live with
// `lib/std/{vec,primitive/{slice,str}}.rs` — exercised by the
// indexing tests under `tests/std/indexing.rs`.

use super::*;

// Positive: a `Range<u32>` literal binds to a let, fields accessed.
#[test]
fn range_field_access_returns_3() {
    expect_answer("lang/ranges/range_field_access", 3i32);
}

// Negative: `..=` without a right side has no semantic shape (there's
// no `RangeFromInclusive` type), so the parser rejects it.
#[test]
fn dotdoteq_without_rhs_is_rejected() {
    let err = compile_source("fn f() -> u32 { let _r = 1u32..=; 0u32 }");
    assert!(
        err.contains("`..=` requires a right-side expression"),
        "expected ..= rejection, got: {}",
        err
    );
}

// Negative: bare `..=` (prefix, no rhs) — same diagnostic.
#[test]
fn prefix_dotdoteq_without_rhs_is_rejected() {
    let err = compile_source("fn f() -> u32 { let _r: u32 = ..=; 0u32 }");
    assert!(
        err.contains("`..=` requires a right-side expression"),
        "expected ..= rejection, got: {}",
        err
    );
}
