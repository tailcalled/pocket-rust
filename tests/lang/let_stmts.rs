// `let`, `let mut`, assignment.

use super::*;

#[test]
fn lets_returns_5() {
    expect_answer("lang/let_stmts/lets", 5i32);
}

// Pattern destructure in `let`. Tuple pattern binds each element
// to its own local: `let (a, b) = pair();` produces two bindings.
#[test]
fn tuple_destructure_returns_42() {
    expect_answer("lang/let_stmts/tuple_destructure", 42i32);
}

// Tuple destructure with `&mut binding` on a leaf. Each leaf needs
// its own addressable storage so writes through the mut ref persist
// when the leaf is later read by name. Used to silently misbehave in
// the Mono codegen path: `pattern_addressed[leaf_id]` wasn't being
// set for destructure bindings, so leaves were stashed in wasm
// locals while the borrow read a separate frame slot.
#[test]
fn tuple_destructure_mut_borrow_returns_129() {
    expect_answer("lang/let_stmts/tuple_destructure_mut_borrow", 129i32);
}

// `let _ = expr;` evaluates `expr` for its side effects and drops
// the value. Useful for explicit unused-result handling.
#[test]
fn wildcard_let_evaluates_for_side_effects() {
    expect_answer("lang/let_stmts/wildcard_let", 42i32);
}

// Negative: refutable patterns (here a literal) require `let-else`.
// The error reuses the match-exhaustiveness machinery via
// `pattern_is_irrefutable`, so any pattern that wouldn't exhaust
// the scrutinee type triggers the same diagnostic.
#[test]
fn refutable_pattern_in_let_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let 0 = 0u32; 42 }",
    );
    assert!(
        err.contains("refutable pattern") && err.contains("let"),
        "expected refutable-pattern error, got: {}",
        err
    );
}

// `let PAT = EXPR else { … };` — pattern matches: bindings scope
// to the rest of the enclosing block.
#[test]
fn let_else_some_returns_42() {
    expect_answer("lang/let_stmts/let_else_some", 42i32);
}

// Pattern doesn't match: else block runs (must diverge — `return`
// here, but `break`/`continue`/`panic!()` would also work).
#[test]
fn let_else_none_returns_42() {
    expect_answer("lang/let_stmts/let_else_none", 42i32);
}

// Negative: let-else's else block must diverge. A non-diverging
// else block (e.g. just an integer expression) errors at typeck
// because the block's type doesn't unify with `!`.
#[test]
fn let_else_non_diverging_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let Option::Some(x) = Option::Some(1u32) else { 0u32 }; x }",
    );
    assert!(
        err.contains("must diverge") || err.contains("`!`"),
        "expected diverging-else-required error, got: {}",
        err
    );
}

#[test]
fn let_mut_scalar_returns_99() {
    expect_answer("lang/let_stmts/let_mut_scalar", 99i32);
}

#[test]
fn let_mut_record_returns_99() {
    expect_answer("lang/let_stmts/let_mut_record", 99i32);
}

#[test]
fn let_mut_nested_returns_99() {
    expect_answer("lang/let_stmts/let_mut_nested", 99i32);
}

#[test]
fn let_annotation_type_mismatch_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f() -> usize { let x: usize = Point { x: 1, y: 2 }; x }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected let-annotation mismatch, got: {}",
        err
    );
}

#[test]
fn let_then_use_after_move_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn use_point(p: Point) -> usize { p.x }\nfn f() -> usize { let p = Point { x: 1, y: 2 }; let q = use_point(p); p.y }",
    );
    assert!(
        err.contains("already moved"),
        "expected use-after-move error, got: {}",
        err
    );
}

#[test]
fn let_out_of_scope_after_block_is_rejected() {
    let err = compile_source("fn f() -> usize { let x = { let y = 7; y }; y }");
    assert!(
        err.contains("unknown variable: `y`"),
        "expected out-of-scope error, got: {}",
        err
    );
}

#[test]
fn assignment_to_immutable_binding_is_rejected() {
    let err = compile_source("fn f() -> u32 { let x = 5; x = 6; x }");
    assert!(
        err.contains("not declared as `mut`"),
        "expected mut-required error, got: {}",
        err
    );
}

#[test]
fn tailless_block_assigned_to_typed_let_is_rejected() {
    // Tail-less blocks evaluate to `()`; binding one to a `usize`-typed
    // let mismatches.
    let err = compile_source("fn f() -> usize { let x: usize = { let y = 5; }; x }");
    assert!(
        err.contains("expected `usize`") && err.contains("got `()`"),
        "expected unit/usize mismatch, got: {}",
        err
    );
}
