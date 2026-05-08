// `pub? const NAME: TYPE = EXPR;` — module-scope compile-time
// constants. The value (a primitive literal) inlines at each
// reference site at codegen.

use super::*;

#[test]
fn const_basic() {
    expect_answer("lang/const_items/basic", 42u32);
}

#[test]
fn const_cross_module() {
    expect_answer("lang/const_items/cross_module", 42u32);
}

#[test]
fn const_initializer_must_be_literal() {
    let err = compile_source(
        "const X: u32 = 21u32 + 21u32; pub fn answer() -> u32 { X }",
    );
    assert!(
        err.contains("primitive literal"),
        "expected `must be a primitive literal` diagnostic, got: {}",
        err,
    );
}
