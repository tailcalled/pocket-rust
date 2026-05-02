// `return EXPR` / `return` (no value): early-exit expression. Type
// `!`. Value unifies against the enclosing function's return type.

use super::*;

#[test]
fn return_value_returns_42() {
    expect_answer("lang/returns/return_value", 42u32);
}

#[test]
fn return_early_returns_42() {
    expect_answer("lang/returns/return_early", 42u32);
}

#[test]
fn return_unit_returns_42() {
    expect_answer("lang/returns/return_unit", 42u32);
}

// Negative: `return EXPR` with a value of the wrong type.
#[test]
fn return_value_type_mismatch_is_rejected() {
    let err = compile_source(
        "fn f() -> u32 { return true; }\n\
         fn answer() -> u32 { f() }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("expected"),
        "expected type-mismatch error, got: {}",
        err
    );
}

// Negative: `return` (no value) inside a function with a non-unit
// return type.
#[test]
fn return_unit_in_value_fn_is_rejected() {
    let err = compile_source(
        "fn f() -> u32 { return; }\n\
         fn answer() -> u32 { f() }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("expected"),
        "expected type-mismatch error, got: {}",
        err
    );
}
