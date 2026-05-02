// `std::ops::{Add, Sub, Mul, Div, Rem, Neg}`: arithmetic operator
// desugar (`+ - * / %` → `<T as Add>::add(self, other)` etc.).
// Numeric literal overloading was dropped — literals only resolve to
// built-in integer types now (no `impl Num for UserType` shortcut).

use super::*;

// Direct primitive-int literals into typed bindings: each literal
// resolves to the binding's annotated kind via context, and codegen
// emits the right `iN.const`. Validates that the literal-only-Int
// constraint still allows mixed-kind let bindings.
#[test]
fn num_literal_dispatch_returns_42() {
    expect_answer("std/num/num_literal_dispatch", 42i32);
}

// `+` operator desugars to `<T as Add>::add(self, other)`. With both
// operands typed u32, dispatch picks `<u32 as Add>::add` which calls
// `¤u32_add`.
#[test]
fn op_arith_returns_42() {
    expect_answer("std/num/op_arith", 42i32);
}

// Asymmetric Mul: `impl Mul<u32> for Vec3 { type Output = Vec3; }`.
// `v * 1u32` dispatches to that impl with Self=Vec3, Rhs=u32. Tests
// that the new generic operator traits handle the case where
// Rhs != Self (the whole motivation for the trait-arg refactor).
#[test]
fn asymmetric_rhs_returns_42() {
    expect_answer("std/num/asymmetric_rhs", 42i32);
}

// Negative: literal overloading dropped. `let x: UserType = 42;`
// where the user's UserType has no special hook should error rather
// than silently routing through some `from_i64`-like trait.
#[test]
fn literal_overloading_into_user_type_is_rejected() {
    let err = compile_source(
        "struct Wrap { v: u32 }\n\
         fn answer() -> u32 { let w: Wrap = 42; w.v }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("expected"),
        "expected type-mismatch error, got: {}",
        err
    );
}
