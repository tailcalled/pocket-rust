// Struct declarations, field access, methods.

use super::*;

#[test]
fn structs_returns_40() {
    expect_answer("lang/structs/structs", 40i32);
}

#[test]
fn methods_returns_42() {
    expect_answer("lang/structs/methods", 42i32);
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
fn no_method_on_struct_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             pt.missing() \
         }",
    );
    assert!(
        err.contains("no method `missing`"),
        "expected no-method error, got: {}",
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
