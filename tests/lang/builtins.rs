// `¤<name>(args)` and `¤<name>::<types>(args)` compiler intrinsics:
// arithmetic / comparison ops on int kinds, the heap (`¤alloc` /
// `¤free`), and pointee-type reinterpretation (`¤cast`).

use super::*;

// `¤<type>_<op>(args)` builtins lower to wasm primitive ops. Here
// `¤u32_add(30, 12)` emits an `i32.add` and returns 42.
#[test]
fn builtin_arith_returns_42() {
    expect_answer("lang/builtins/builtin_arith", 42i32);
}

// Comparison builtin in if-condition: `¤i32_lt(5, 7)` is true, so
// the if returns 11.
#[test]
fn builtin_cmp_in_if_returns_11() {
    expect_answer("lang/builtins/builtin_cmp_in_if", 11i32);
}

// 128-bit arithmetic and comparison: both args sum to over 2^63 so
// the addition carries into the high half, exercising the carry
// emission. Equality and lt builtins on u128 also tested.
#[test]
fn builtin_u128_returns_11() {
    expect_answer("lang/builtins/builtin_u128", 11i32);
}

#[test]
fn heap_alloc_write_read_returns_42() {
    // `¤alloc(4)` → `*mut u8`; `¤cast::<u32, u8>` reinterprets as
    // `*mut u32`; write 42, read back, free.
    expect_answer("lang/builtins/heap_alloc_write_read", 42u32);
}

#[test]
fn heap_cast_pointee_returns_truncated() {
    // 9_000_000_000 stored as u64 through `*mut u64`, cast back
    // through `*mut u8` and up to `*mut u64`, reload, truncate to
    // u32 = 9_000_000_000 mod 2^32 = 410_065_408.
    expect_answer("lang/builtins/heap_cast_pointee", 410_065_408u32);
}

#[test]
fn heap_alloc_struct_returns_42() {
    // 8-byte alloc, cast to `*mut Point`, store Point { x: 7, y:
    // 35 }, sum the fields → 42.
    expect_answer("lang/builtins/heap_alloc_struct", 42u32);
}

#[test]
fn unknown_builtin_is_rejected() {
    let err = compile_source("fn answer() -> u32 { ¤u32_unknown(1, 2) }");
    assert!(
        err.contains("unknown builtin"),
        "expected unknown-builtin error, got: {}",
        err
    );
}

#[test]
fn builtin_wrong_arg_count_is_rejected() {
    let err = compile_source("fn answer() -> u32 { ¤u32_add(1) }");
    assert!(
        err.contains("argument"),
        "expected wrong-arity error, got: {}",
        err
    );
}

#[test]
fn builtin_arg_type_mismatch_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let a: u32 = 1; let b: u64 = 2; ¤u32_add(a, b) }",
    );
    assert!(
        err.contains("type mismatch"),
        "expected type-mismatch error, got: {}",
        err
    );
}

#[test]
fn builtin_cast_without_turbofish_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { unsafe { let p: *mut u8 = ¤alloc(4); let _q = ¤cast(p); 0 } }",
    );
    assert!(
        err.contains("type argument"),
        "expected missing-turbofish error, got: {}",
        err
    );
}

#[test]
fn builtin_cast_argument_must_be_raw_pointer() {
    let err = compile_source(
        "fn answer() -> u32 { let x: u32 = 5; let _q: *mut u32 = ¤cast::<u32, u32>(x); 0 }",
    );
    assert!(
        err.contains("raw pointer"),
        "expected non-raw-pointer error, got: {}",
        err
    );
}

#[test]
fn builtin_alloc_does_not_take_type_arguments() {
    let err = compile_source(
        "fn answer() -> u32 { unsafe { let p: *mut u8 = ¤alloc::<u8>(4); ¤free(p); 0 } }",
    );
    assert!(
        err.contains("type argument"),
        "expected no-type-args error, got: {}",
        err
    );
}
