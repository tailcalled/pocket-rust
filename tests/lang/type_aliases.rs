// Tests for top-level `pub? type Name<...>? = TypeExpr;` declarations.
// Aliases are fully transparent — `Foo` and its target are
// interchangeable everywhere a type is expected.

use super::*;

#[test]
fn type_alias_primitive() {
    expect_answer("lang/type_aliases/primitive_alias", 42u32);
}

#[test]
fn type_alias_in_signature() {
    expect_answer("lang/type_aliases/in_signature", 42u32);
}

#[test]
fn type_alias_in_struct_field() {
    expect_answer("lang/type_aliases/in_struct_field", 42u32);
}

#[test]
fn type_alias_cross_module_with_use_rename() {
    // module `a` declares `pub type X = u32`; module `b` imports it
    // via `use crate::a::X as Y;` and uses `Y` in a function
    // signature. Exercises (1) `pub` visibility on aliases,
    // (2) `use ... as` renaming for an alias, and (3) cross-module
    // path resolution all the way through to the alias's target.
    expect_answer("lang/type_aliases/cross_module_alias", 42u32);
}

#[test]
fn type_alias_unknown_target_rejected() {
    let err = compile_source("type Foo = NotAType;");
    assert!(err.contains("unknown type"), "got: {}", err);
}

#[test]
fn type_alias_missing_semicolon_rejected() {
    let err = compile_source("type Foo = u32");
    assert!(err.contains("`;`") || err.contains("semicolon"), "got: {}", err);
}
