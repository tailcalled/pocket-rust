// `let`, `let mut`, assignment.

use super::*;

#[test]
fn lets_returns_5() {
    expect_answer("lang/let_stmts/lets", 5i32);
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
