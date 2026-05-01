// Generic functions / structs / impls.

use super::*;

#[test]
fn generic_id_returns_100() {
    expect_answer("lang/generics/generic_id", 100i32);
}

#[test]
fn generic_pair_returns_7() {
    expect_answer("lang/generics/generic_pair", 7i32);
}

#[test]
fn wrong_struct_type_arg_count_is_rejected() {
    let err = compile_source(
        "struct Pair<T, U> { first: T, second: U }\n\
         fn answer() -> u32 { let p: Pair<u32> = Pair { first: 1, second: 2 }; p.first }",
    );
    assert!(
        err.contains("type arguments"),
        "expected wrong-struct-type-arg-count error, got: {}",
        err
    );
}

#[test]
fn field_access_on_generic_param_is_rejected() {
    // Polymorphic body check: `t.field` where `t: T` has no shape — reject.
    let err = compile_source(
        "fn bad<T>(t: T) -> u32 { t.field }",
    );
    assert!(
        err.contains("non-struct"),
        "expected field-access-on-T error, got: {}",
        err
    );
}

#[test]
fn turbofish_on_non_generic_is_rejected() {
    let err = compile_source(
        "fn plain() -> u32 { 7 }\n\
         fn answer() -> u32 { plain::<u32>() }",
    );
    assert!(
        err.contains("not a generic function") || err.contains("turbofish"),
        "expected turbofish-on-non-generic error, got: {}",
        err
    );
}

#[test]
fn wrong_type_arg_count_is_rejected() {
    let err = compile_source(
        "fn id<T>(x: T) -> T { x }\n\
         fn answer() -> u32 { id::<u32, u64>(5) }",
    );
    assert!(
        err.contains("type arguments"),
        "expected wrong-type-arg-count error, got: {}",
        err
    );
}
