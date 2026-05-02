// Short-circuiting boolean operators `&&` / `||` and prefix `!`.
// All three desugar at parse time:
//   `a && b` → `if a { b } else { false }`
//   `a || b` → `if a { true } else { b }`
//   `!a`     → `a.not()` (via `std::ops::Not`)

use super::*;

#[test]
fn logical_and_returns_42() {
    expect_answer("lang/operators/logical_and", 42i32);
}

#[test]
fn logical_or_returns_42() {
    expect_answer("lang/operators/logical_or", 42i32);
}

#[test]
fn logical_not_returns_42() {
    expect_answer("lang/operators/logical_not", 42i32);
}

// Short-circuit: `a && rhs` skips `rhs` when `a` is false. Verified
// by putting `panic!()` on the rhs — if `&&` were strict the example
// would trap.
#[test]
fn short_circuit_and_returns_42() {
    expect_answer("lang/operators/short_circuit_and", 42i32);
}

// Short-circuit: `a || rhs` skips `rhs` when `a` is true.
#[test]
fn short_circuit_or_returns_42() {
    expect_answer("lang/operators/short_circuit_or", 42i32);
}

// Negative: `!` on a non-Not type errors via the trait dispatch
// path. Tuples don't have a `Not` impl → "no method `not` on …".
#[test]
fn logical_not_on_non_not_type_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let t: (u32, u32) = (1, 2); if !t { 1 } else { 0 } }",
    );
    assert!(
        err.contains("no method `not`"),
        "expected no-method error, got: {}",
        err
    );
}
