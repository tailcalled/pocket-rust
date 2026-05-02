// Compound-assignment operators (`+=`, `-=`, `*=`, `/=`, `%=`).
// Each parses to `Stmt::Expr(MethodCall { method: "<op>_assign",
// receiver: lhs, args: [rhs] })`. Method dispatch autorefs the
// receiver to `&mut Self`, so the LHS must be a mutable place.

use super::*;

// `+=` and `+=` in a loop. Validates the parser accepts the
// compound op tokens and dispatch routes through `AddAssign::add_assign`
// for u32.
#[test]
fn plus_eq_returns_42() {
    expect_answer("lang/compound_assign/plus_eq", 42i32);
}

// `*=`, `-=`, `/=`, `%=` in sequence. Exercises the rest of the
// `*Assign` family.
#[test]
fn times_eq_returns_42() {
    expect_answer("lang/compound_assign/times_eq", 42i32);
}

// Negative: `+=` on an immutable binding. Method dispatch needs to
// autoref to `&mut Self`; without a mutable place the autoref-mut
// level isn't tried, leaving no matching candidate (`add_assign`
// takes `&mut Self`, not owned `Self`). Surfaces as "no method".
#[test]
fn plus_eq_on_immutable_binding_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: u32 = 0; x += 1; x }",
    );
    assert!(
        err.contains("no method `add_assign`"),
        "expected no-method error, got: {}",
        err
    );
}

// Negative: type mismatch on RHS — `+=` between a u32 and a u8
// has no impl (cross-kind requires explicit cast).
#[test]
fn plus_eq_cross_kind_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let mut x: u32 = 0; let y: u8 = 1; x += y; x }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("expected") || err.contains("no impl"),
        "expected type-mismatch error, got: {}",
        err
    );
}
