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

// `let x: T;` (no initializer) — declaration only. Borrowck seeds
// the binding as `Uninit`; an assignment clears it back to `Init`,
// then a read passes.
#[test]
fn uninit_then_assign_returns_99() {
    expect_answer("lang/let_stmts/uninit_then_assign", 99u32);
}

// Uninit let + if/else assigning on every path → join point is
// Init, so the trailing read is accepted.
#[test]
fn uninit_assign_in_both_branches_returns_42() {
    expect_answer("lang/let_stmts/uninit_assign_in_both_branches", 42u32);
}

// Negative: read before any assignment errors via the
// move-state lattice's `Uninit` state.
#[test]
fn uninit_read_before_assign_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: u32; x }",
    );
    assert!(
        err.contains("uninitialized") && err.contains("`x`"),
        "expected uninit-read diagnostic, got: {}",
        err
    );
}

// Negative: assignment on only one branch leaves the join in
// MaybeMoved state; the trailing read errors. (Today the diagnostic
// reuses the moved-on-some-paths message; richer "maybe-uninit"
// wording is a future polish.)
#[test]
fn uninit_assign_in_only_one_branch_is_rejected() {
    let err = compile_source(
        "fn answer(b: bool) -> u32 { let x: u32; if b { x = 1u32; } x }",
    );
    assert!(
        err.contains("`x` was already moved") || err.contains("uninitialized"),
        "expected possibly-uninit error on one-armed assignment, got: {}",
        err
    );
}

// `let x;` — no annotation, no initializer. The binding's type is
// inferred from the later assignment via a fresh InferType::Var that
// typeck seeds when neither annotation nor value is present, and the
// assignment unifies with the RHS's type.
#[test]
fn uninit_no_annotation_returns_99() {
    expect_answer("lang/let_stmts/uninit_no_annotation", 99u32);
}

// Negative: tuple destructure has nothing to destructure when the
// initializer is absent.
#[test]
fn uninit_with_destructure_pattern_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let (a, b): (u32, u32); a + b }",
    );
    assert!(
        err.contains("single binding pattern"),
        "expected destructure-rejection error, got: {}",
        err
    );
}

// Negative: an uninit let with a let-else block is meaningless —
// no scrutinee to test against.
#[test]
fn uninit_with_else_block_is_rejected() {
    // Parser rejects this earlier (else is only consumed when `=`
    // is present), so the user-visible failure is "expected `;`,
    // got `else`".
    let err = compile_source(
        "fn answer() -> u32 { let x: u32 else { return 0u32; }; 0u32 }",
    );
    assert!(
        err.contains("expected `;`") || err.contains("else"),
        "expected parse-error on uninit + else, got: {}",
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

// `fn f(mut x: T)` — the parameter binding is mutable so the body
// can re-assign through `x`. Mirrors `let mut x = …; x = …`.
#[test]
fn mut_param_reassign_returns_99() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn bump(mut x: u32) -> u32 { x = 99; x }\nfn answer() -> u32 { bump(1) }",
        )],
        99u32,
    );
}

// `mut` on the parameter is what enables compound-assign (`x += 1`)
// against the binding itself.
#[test]
fn mut_param_compound_assign_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn bump(mut x: u32) -> u32 { x += 1; x += 1; x }\nfn answer() -> u32 { bump(40) }",
        )],
        42u32,
    );
}

// `mut self` on a method receiver — by-value receiver with mutable
// binding. Re-assigning `self` inside the method is allowed and the
// returned value reflects the assignment.
#[test]
fn mut_self_receiver_reassign_returns_77() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "struct S { v: u32 }\nimpl S { fn bump(mut self) -> u32 { self = S { v: 77 }; self.v } }\nfn answer() -> u32 { S { v: 1 }.bump() }",
        )],
        77u32,
    );
}

// Negative: an immutable parameter cannot be assigned to. Same error
// shape as `let x = …; x = …` would produce.
#[test]
fn assignment_to_immutable_param_is_rejected() {
    let err = compile_source("fn f(x: u32) -> u32 { x = 6; x }\nfn answer() -> u32 { f(1) }");
    assert!(
        err.contains("not declared as `mut`"),
        "expected mut-required error for non-mut param, got: {}",
        err
    );
}

// Negative: compound assignment to a non-mut param is rejected. `x +=
// 1` desugars to a method call on `&mut x`; autoref-mut is only tried
// against a mutable place, so the missing-method diagnostic surfaces
// (mirrors the same shape as `let x: u32 = 0; x += 1`).
#[test]
fn compound_assign_to_immutable_param_is_rejected() {
    let err = compile_source("fn f(x: u32) -> u32 { x += 1; x }\nfn answer() -> u32 { f(1) }");
    assert!(
        err.contains("no method `add_assign`"),
        "expected no-method error for compound-assign on non-mut param, got: {}",
        err
    );
}
