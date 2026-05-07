// `_` in type position — placeholder that resolves to a fresh
// inference variable. Allowed at turbofish call sites and `let`
// annotations; rejected everywhere else.

use super::*;

// Turbofish `_` on a function call: the type-arg slot is left to
// inference, which is pinned by the value-arg.
#[test]
fn turbofish_placeholder_on_fn_call_returns_42() {
    let bytes = compile_inline(
        "fn id<T>(x: T) -> T { x }\n\
         pub fn answer() -> u32 { id::<_>(42u32) }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

// Turbofish with mixed concrete + `_` on a function call.
#[test]
fn turbofish_partial_placeholder_returns_30() {
    let bytes = compile_inline(
        "fn pair<A, B>(a: A, _b: B) -> A { a }\n\
         pub fn answer() -> u32 { pair::<u32, _>(30u32, 99u64) }",
    );
    assert_eq!(answer_u32(&bytes), 30);
}

// `let x: _` — tail placeholder. The value type fully pins the
// inferred placeholder.
#[test]
fn let_annotation_bare_placeholder_returns_42() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let x: _ = 42u32; x }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

// `let x: Vec<_>` — placeholder nested inside a generic struct.
// (Uses Option here since std vec needs more setup; same machinery.)
#[test]
fn let_annotation_nested_placeholder_returns_7() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let x: Option<_> = Option::Some(7u32); match x { Option::Some(v) => v, Option::None => 0u32 } }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

// Negative: `_` in a function return type is rejected.
#[test]
fn placeholder_in_return_type_is_rejected() {
    let err = compile_source(
        "fn f() -> _ { 0u32 }\nfn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("type placeholder `_`"),
        "expected placeholder-not-allowed error, got: {}",
        err,
    );
}

// Negative: `_` in a struct field type is rejected.
#[test]
fn placeholder_in_struct_field_is_rejected() {
    let err = compile_source(
        "struct S { x: _ }\nfn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("type placeholder `_`"),
        "expected placeholder-not-allowed error, got: {}",
        err,
    );
}

// Negative: `_` in a function parameter type is rejected.
#[test]
fn placeholder_in_fn_param_is_rejected() {
    let err = compile_source(
        "fn f(x: _) -> u32 { x }\nfn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("type placeholder `_`"),
        "expected placeholder-not-allowed error, got: {}",
        err,
    );
}
