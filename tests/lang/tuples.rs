// Tuples + the unit type `()`.

use super::*;

// Tuples + unit type. Construction, indexing, nested, unit value
// `()` as a tail-less return type, expression statements that
// discard their value, and tuple-field assignment via `t.0 = …`.
#[test]
fn tuple_basic_returns_42() {
    expect_answer("lang/tuples/tuple_basic", 42u32);
}

#[test]
fn tuple_index_returns_42() {
    expect_answer("lang/tuples/tuple_index", 42u32);
}

#[test]
fn tuple_unit_returns_42() {
    expect_answer("lang/tuples/tuple_unit", 42u32);
}

#[test]
fn tuple_nested_returns_42() {
    expect_answer("lang/tuples/tuple_nested", 42u32);
}

#[test]
fn tuple_expr_stmt_returns_42() {
    expect_answer("lang/tuples/tuple_expr_stmt", 42u32);
}

#[test]
fn tuple_field_assign_returns_42() {
    expect_answer("lang/tuples/tuple_field_assign", 42u32);
}

#[test]
fn tuple_borrow_returns_42() {
    expect_answer("lang/tuples/tuple_borrow", 42u32);
}

#[test]
fn tuple_index_out_of_range_is_rejected() {
    let err = compile_source("fn f() -> u32 { let t: (u32, u32) = (1, 2); t.5 }");
    assert!(
        err.contains("tuple index 5 out of range"),
        "expected tuple-index out-of-range error, got: {}",
        err
    );
}

#[test]
fn tuple_index_on_non_tuple_is_rejected() {
    let err = compile_source("fn f() -> u32 { let x: u32 = 5; x.0 }");
    assert!(
        err.contains("tuple index `.0` on non-tuple type"),
        "expected non-tuple tuple-index error, got: {}",
        err
    );
}

#[test]
fn tuple_unit_used_as_value_is_rejected() {
    let err = compile_source("fn f() -> u32 { let x: u32 = (); x }");
    assert!(
        err.contains("expected `u32`") && err.contains("got `()`"),
        "expected u32/unit mismatch, got: {}",
        err
    );
}
