// Enum declarations, variants (unit/tuple/struct), generic enums.

use super::*;

#[test]
fn enum_decl_parses_returns_42() {
    expect_answer("lang/enums/enum_decl_parses", 42u32);
}

#[test]
fn enum_unit_variant_returns_42() {
    expect_answer("lang/enums/enum_unit_variant", 42u32);
}

#[test]
fn enum_tuple_variant_returns_42() {
    expect_answer("lang/enums/enum_tuple_variant", 42u32);
}

#[test]
fn enum_struct_variant_returns_42() {
    expect_answer("lang/enums/enum_struct_variant", 42u32);
}

#[test]
fn enum_generic_returns_42() {
    expect_answer("lang/enums/enum_generic", 42u32);
}

#[test]
fn enum_pass_to_fn_returns_42() {
    expect_answer("lang/enums/enum_pass_to_fn", 42u32);
}

#[test]
fn enum_return_returns_42() {
    expect_answer("lang/enums/enum_return", 42u32);
}
