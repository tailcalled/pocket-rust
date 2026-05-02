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
