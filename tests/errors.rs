use pocket_rust::{Vfs, compile};

fn compile_source(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    compile(&[], &vfs, "lib.rs").err().expect("expected error")
}

#[test]
fn lex_error_reports_line_and_column() {
    let err = compile_source("fn answer() -> usize { @ }");
    assert!(
        err.starts_with("lib.rs:1:24:"),
        "expected `lib.rs:1:24:` prefix, got: {}",
        err
    );
}

#[test]
fn parse_error_reports_line_and_column() {
    let err = compile_source("fn ok() -> usize { 42 }\nfn bad)\n");
    assert!(
        err.starts_with("lib.rs:2:7:"),
        "expected `lib.rs:2:7:` prefix, got: {}",
        err
    );
}

#[test]
fn codegen_error_reports_line_and_column() {
    let err = compile_source("fn big() -> usize { 99999999999 }");
    assert!(
        err.starts_with("lib.rs:1:21:"),
        "expected `lib.rs:1:21:` prefix, got: {}",
        err
    );
}

#[test]
fn missing_module_file_reports_decl_site() {
    let err = compile_source("mod nope;\nfn f() {}");
    assert!(
        err.starts_with("lib.rs:1:5:"),
        "expected `lib.rs:1:5:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("nope.rs"),
        "expected message to mention `nope.rs`, got: {}",
        err
    );
}

#[test]
fn unresolved_call_reports_call_site() {
    let err = compile_source("fn main() -> usize { ghost::missing() }");
    assert!(
        err.starts_with("lib.rs:1:22:"),
        "expected `lib.rs:1:22:` prefix, got: {}",
        err
    );
}

#[test]
fn unknown_variable_reports_use_site() {
    let err = compile_source("fn f(a: usize) -> usize { b }");
    assert!(
        err.starts_with("lib.rs:1:27:"),
        "expected `lib.rs:1:27:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("unknown variable"),
        "expected message about unknown variable, got: {}",
        err
    );
}

#[test]
fn arity_mismatch_reports_call_site() {
    let err = compile_source(
        "fn id(x: usize) -> usize { x }\nfn caller() -> usize { id(1, 2) }",
    );
    assert!(
        err.starts_with("lib.rs:2:24:"),
        "expected `lib.rs:2:24:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("expected 1, got 2"),
        "expected arity mismatch detail, got: {}",
        err
    );
}

#[test]
fn unknown_struct_field_reports_use_site() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f(p: Point) -> usize { p.z }",
    );
    assert!(
        err.starts_with("lib.rs:2:29:"),
        "expected `lib.rs:2:29:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("no field `z`"),
        "expected `no field z` detail, got: {}",
        err
    );
}

#[test]
fn missing_struct_field_in_literal() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f() -> Point { Point { x: 1 } }",
    );
    assert!(
        err.contains("missing field `y`"),
        "expected missing field detail, got: {}",
        err
    );
}

#[test]
fn duplicate_partial_move_is_rejected() {
    let err = compile_source(
        "struct Pair { a: usize, b: usize }\nstruct Outer { p: Pair, q: Pair }\nfn f(o: Outer) -> Pair { Pair { a: o.p.a, b: o.p.a } }",
    );
    assert!(
        err.contains("already moved"),
        "expected move error, got: {}",
        err
    );
}

#[test]
fn whole_after_partial_move_is_rejected() {
    let err = compile_source(
        "struct Pair { a: usize, b: usize }\nfn use_pair(p: Pair) -> usize { p.a }\nfn f(p: Pair) -> usize { use_pair(Pair { a: p.a, b: p.a }) }",
    );
    assert!(
        err.contains("already moved"),
        "expected move error, got: {}",
        err
    );
}

#[test]
fn arg_type_mismatch_struct_for_usize() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn id(n: usize) -> usize { n }\nfn f(p: Point) -> usize { id(p) }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected arg-type mismatch detail, got: {}",
        err
    );
}

#[test]
fn arg_type_mismatch_usize_for_struct() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn use_point(p: Point) -> usize { p.x }\nfn f() -> usize { use_point(7) }",
    );
    assert!(
        err.contains("expected `Point`"),
        "expected arg-type mismatch mentioning `Point`, got: {}",
        err
    );
}

#[test]
fn struct_field_init_type_mismatch() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Pair { a: Point, b: Point }\nfn f() -> Pair { Pair { a: 1, b: 2 } }",
    );
    assert!(
        err.contains("expected `Point`, got integer"),
        "expected field-type mismatch, got: {}",
        err
    );
}

#[test]
fn return_type_mismatch() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn make() -> Point { Point { x: 1, y: 2 } }\nfn f() -> usize { make() }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected return-type mismatch, got: {}",
        err
    );
}

#[test]
fn field_access_on_usize_is_rejected() {
    let err = compile_source(
        "fn id(n: usize) -> usize { n }\nfn f() -> usize { id(7).x }",
    );
    assert!(
        err.contains("non-struct"),
        "expected non-struct field-access error, got: {}",
        err
    );
}

#[test]
fn move_while_borrowed_is_rejected() {
    // Two args of the same call: a borrow of `p` and a move out of `p`.
    // The borrow is still active when the second arg is evaluated, so the move conflicts.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn use_borrow(p: &Point, q: usize) -> usize { q }\nfn bad(p: Point) -> usize { use_borrow(&p, p.y) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn borrow_after_move_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn x_of(p: &Point) -> usize { p.x }\nfn first(a: usize, b: usize) -> usize { a }\nfn bad(p: Point) -> usize { first(p.x, x_of(&p)) }",
    );
    assert!(
        err.contains("moved"),
        "expected borrow-after-move error, got: {}",
        err
    );
}

#[test]
fn move_out_of_borrow_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Rect { tl: Point, br: Point }\nfn whoops(r: &Rect) -> Point { r.tl }",
    );
    assert!(
        err.contains("cannot move out of borrow"),
        "expected move-out-of-borrow error, got: {}",
        err
    );
}

#[test]
fn ref_in_struct_field_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Bad { p: &Point }",
    );
    assert!(
        err.contains("struct fields cannot have reference types"),
        "expected struct-field-ref error, got: {}",
        err
    );
}

#[test]
fn ref_return_type_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn whoops(p: &Point) -> &Point { p }",
    );
    assert!(
        err.contains("cannot return reference"),
        "expected ref-return error, got: {}",
        err
    );
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
fn block_expr_without_tail_is_rejected() {
    let err = compile_source("fn f() -> usize { let x = { let y = 5; }; x }");
    assert!(
        err.contains("block expression must end with"),
        "expected tailless block-expr error, got: {}",
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
fn field_assignment_to_immutable_record_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn f() -> u32 { let p = Point { x: 1, y: 2 }; p.x = 99; p.x }",
    );
    assert!(
        err.contains("not declared as `mut`"),
        "expected mut-required error, got: {}",
        err
    );
}

#[test]
fn assignment_while_borrowed_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn x_of(p: &Point) -> u32 { p.x }\nfn use_borrow(p: &Point, q: u32) -> u32 { q }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; let r = &p; use_borrow(r, { p.x = 99; p.x }) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected borrow-conflict error, got: {}",
        err
    );
}

#[test]
fn integer_literal_too_big_for_u8() {
    let err = compile_source("fn f() -> u8 { 300 }");
    assert!(
        err.contains("does not fit"),
        "expected fit-check error, got: {}",
        err
    );
}

#[test]
fn integer_literal_defaults_to_i32() {
    // `x` is never used, so its type variable is unconstrained and defaults to
    // i32. 4_000_000_000 doesn't fit in i32, so the post-solve range check
    // catches it — proving the default fired.
    let err = compile_source("fn f() -> u32 { let x = 4000000000; 0 }");
    assert!(
        err.contains("does not fit"),
        "expected default-overflow error, got: {}",
        err
    );
}

#[test]
fn borrow_through_inner_block_blocks_outer_move() {
    // The borrow `&pt1` is created inside the inner block, but the block returns
    // it (as `pt3`) so the borrow ends up bound to `pt2`. A subsequent move of
    // `pt1` must be rejected — `pt2` would otherwise be a dangling reference.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn x_of(p: &Point) -> usize { p.x }\nfn f() -> usize { let pt1 = Point { x: 42, y: 0 }; let pt2 = { let pt3 = &pt1; pt3 }; let invalid = pt1; x_of(pt2) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn borrow_of_subfield_blocks_parent_move() {
    // `&p.x` borrows the subfield, leaving `p` with a borrowed sub-place.
    // Trying to move `p` whole then has to fail.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f(p: Point) -> usize { let r = &p.x; let q = p; q.y }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

