// Tests for `#[deriving(...)]` synthesis. Each test exercises a
// derive against a struct or enum and checks observable behavior of
// the synthesized impl.

use super::*;

#[test]
fn derive_clone_struct_returns_equal_clone() {
    expect_answer("lang/derives/struct_clone_eq", 42u32);
}

#[test]
fn derive_partial_ord_struct_lexicographic() {
    expect_answer("lang/derives/struct_partial_ord", 42u32);
}

#[test]
fn derive_copy_eq_struct() {
    expect_answer("lang/derives/struct_copy_eq", 42u32);
}

#[test]
fn derive_clone_eq_enum() {
    expect_answer("lang/derives/enum_clone_eq", 42u32);
}

#[test]
fn derive_generic_struct() {
    expect_answer("lang/derives/struct_generic", 42u32);
}

#[test]
fn derive_unit_struct() {
    expect_answer("lang/derives/struct_unit", 42u32);
}

#[test]
fn derive_unknown_trait_rejected() {
    let err = compile_source("#[deriving(Foo)] struct S { x: u32 }");
    assert!(err.contains("cannot derive"), "got: {}", err);
}

#[test]
fn derive_partial_ord_on_enum_rejected() {
    let err = compile_source("#[deriving(PartialOrd)] enum E { A, B }");
    assert!(err.contains("cannot derive `PartialOrd`"), "got: {}", err);
}

#[test]
fn derive_attribute_on_fn_rejected() {
    let err = compile_source("#[deriving(Clone)] fn foo() {}");
    assert!(
        err.contains("only allowed on `struct` or `enum`"),
        "got: {}",
        err,
    );
}

#[test]
fn derive_unknown_attribute_rejected() {
    let err = compile_source("#[suspicion(Clone)] struct S { x: u32 }");
    assert!(err.contains("unknown attribute"), "got: {}", err);
}
