// `for pat in iter { body }` — iterates `iter` (must impl
// `std::iter::Iterator`) by repeatedly calling `next()` until None.
// The for-loop stays as a first-class node through typeck (so
// errors mention `for`); borrowck and codegen handle it directly,
// modelled on a `loop { match next() { Some(pat) => body, None =>
// break } }` shape.

use super::*;

#[test]
fn counter_basic_returns_42() {
    expect_answer("lang/for_loops/counter_basic", 42i32);
}

#[test]
fn counter_break_returns_42() {
    expect_answer("lang/for_loops/counter_break", 42i32);
}

#[test]
fn counter_continue_returns_42() {
    expect_answer("lang/for_loops/counter_continue", 42i32);
}

#[test]
fn counter_label_returns_42() {
    expect_answer("lang/for_loops/counter_label", 42i32);
}

#[test]
fn for_wildcard_returns_42() {
    expect_answer("lang/for_loops/for_wildcard", 42i32);
}

// For-loop desugar's synth `__iter` binding must end up in
// `Storage::MemoryAt` (dynamic shadow-stack slot) — `&mut __iter` is
// taken to call `Iterator::next`, so the binding needs an address.
// Regression test for codegen_mono_stmt::Let's MemoryAt branch:
// without it (e.g. if the layout pass marked __iter as Local), the
// `&mut __iter` borrow yields a wasm-locals address that doesn't
// exist, the iterator state never advances across iterations, and the
// loop either runs forever (n stays at start) or terminates after one
// iter (n stale). 10 + 14 + 18 = 42 verifies in-place mutation
// survives across iterations.
#[test]
fn iter_binding_addressed_returns_42() {
    expect_answer("lang/for_loops/iter_binding_addressed", 42i32);
}

// Negative: iter expression's type doesn't impl Iterator. Error
// should mention `for` and Iterator (typeck sees ForLoop directly).
#[test]
fn for_non_iterator_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { for _ in 5u32 { } 42 }",
    );
    assert!(
        err.contains("Iterator") && err.contains("for"),
        "expected Iterator-not-implemented-by-for error, got: {}",
        err
    );
}

#[test]
fn single_label_returns_42() {
    expect_answer("lang/for_loops/single_label", 42i32);
}
