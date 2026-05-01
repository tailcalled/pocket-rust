// `std::ops::Num`: integer-literal dispatch through `from_i64`,
// arithmetic operator desugar (`+ - * / %` → `<T as Num>::add`...).

use super::*;

// T5: every integer literal desugars to `<T as Num>::from_i64(value)`.
// This test exercises u8, i64, and u32 literal codegen end-to-end —
// each literal becomes a real call to the relevant `from_i64` impl
// (no inlining), and the values flow through to the answer.
#[test]
fn num_literal_dispatch_returns_42() {
    expect_answer("std/num/num_literal_dispatch", 42i32);
}

// T5.5: integer literal lands on a user type via `impl Num for Wrap`.
// `let w: Wrap = 42` resolves the literal to `Wrap` (instead of
// erroring because Wrap isn't an integer kind), and codegen routes
// through `<Wrap as Num>::from_i64`.
#[test]
fn num_user_type_returns_42() {
    expect_answer("std/num/num_user_type", 42i32);
}

// T5.5: integer literal in a `<T: Num>` generic body. Inside `make<T:
// Num>() -> T { 42 }`, the literal lands on `T` (Param-typed); at
// mono time `T` resolves to `u32`, and the literal codegens as
// `<u32 as Num>::from_i64(42)`.
#[test]
fn num_generic_body_returns_42() {
    expect_answer("std/num/num_generic_body", 42i32);
}

// `+` operator desugars to `<T as Num>::add(self, other)`. 30 + 12
// dispatches via `<u32 as Num>::add` which calls `¤u32_add`.
#[test]
fn op_arith_returns_42() {
    expect_answer("std/num/op_arith", 42i32);
}
