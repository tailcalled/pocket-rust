// `if`/`else` value expressions: result types (single-value vs.
// multi-value), conditional Drop, generic `T` flowing through arms.

use super::*;

// Booleans + if-expression: `if b { 1 } else { 2 }` with `b: bool`.
// Verifies bool literal codegen, the wasm if/else block, and bool
// flow through a function parameter.
#[test]
fn bool_if_returns_1() {
    expect_answer("lang/if_exprs/bool_if", 1i32);
}

// Multi-value `if` result: a u128 flattens to two i64s, so the
// wasm if/else block must reference a registered FuncType (no
// params, two i64 results) by typeidx. Codegen registers it on the
// fly via `pending_types`, drained into wasm_mod.types at function-
// emit-end. Returns 42u128 = (low=42, high=0).
#[test]
fn if_returns_u128() {
    expect_answer("lang/if_exprs/if_returns_u128", (42i64, 0i64));
}

// Multi-value `if` returning a struct that flattens to (i32, i64).
#[test]
fn if_returns_struct() {
    expect_answer("lang/if_exprs/if_returns_struct", 9_000_000_000i64);
}

// Conditional Drop: `l: Logger` is moved into `consume(l)` in the
// then-arm but not the else-arm. Borrowck records its post-merge
// status as MaybeMoved; codegen allocates a runtime drop flag.
#[test]
fn if_conditional_drop_returns_5() {
    expect_answer("lang/if_exprs/if_conditional_drop", 5u32);
}

// Drop binding moved in BOTH arms — borrowck's intersection rule
// gives final status `Moved` (not MaybeMoved).
#[test]
fn if_drop_moved_in_both_returns_5() {
    expect_answer("lang/if_exprs/if_drop_moved_in_both", 5u32);
}

// Drop binding moved in NEITHER arm — status is `Init` post-merge.
#[test]
fn if_drop_moved_in_neither_returns_5() {
    expect_answer("lang/if_exprs/if_drop_moved_in_neither", 5u32);
}

// Borrows flow through if-tail. Both arms produce `&'a u32`; the if-
// expression's value carries the union of arm borrows so the caller's
// let-binding correctly tracks borrows on both possible sources.
#[test]
fn if_returns_borrow_returns_42() {
    expect_answer("lang/if_exprs/if_returns_borrow", 42i32);
}

// Generic `T` flowing through an if. `pick<T>(b, x, y) -> T` walks
// polymorphically; codegen monomorphizes per call site.
#[test]
fn if_generic_t_returns_42() {
    expect_answer("lang/if_exprs/if_generic_t", 42i32);
}

// Same generic if-pick, but monomorphized to `u128`.
#[test]
fn if_generic_t_u128_returns_42() {
    expect_answer("lang/if_exprs/if_generic_t_u128", (42i64, 0i64));
}

#[test]
fn if_without_else_returns_42() {
    expect_answer("lang/if_exprs/if_without_else", 42u32);
}

#[test]
fn if_condition_must_be_bool() {
    let err = compile_source("fn answer() -> u32 { if 1 { 1 } else { 2 } }");
    assert!(
        err.contains("bool") || err.contains("type mismatch"),
        "expected non-bool condition error, got: {}",
        err
    );
}

#[test]
fn if_arms_must_unify() {
    let err = compile_source(
        "fn answer() -> u32 { if true { 1u32 as u32 } else { 0u64 as u64 } }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("expected"),
        "expected arm-mismatch error, got: {}",
        err
    );
}
