// Integer literal lexing, type inference, and the `Num`-defaulting / range-check pipeline.
//
// Tests of the `Num` trait *implementation* (literals dispatching through
// `<T as Num>::from_i64`) live in `tests/std/num.rs`. This file covers
// purely-language-level concerns: literal parsing, defaulting, range
// errors, and inference under generic bounds.

use super::*;

#[test]
fn u8_literal_returns_200() {
    expect_answer("lang/int_literals/u8_literal", 200i32);
}

#[test]
fn i64_literal_returns_9_000_000_000() {
    expect_answer("lang/int_literals/i64_literal", 9_000_000_000i64);
}

// 128-bit literal goes through `<u128 as Num>::from_i64` which casts
// the i64 argument to u128 — exercising the Wide64 → Wide128 path
// (zero-extending the high half for unsigned target).
#[test]
fn u128_literal_returns_42() {
    expect_answer("lang/int_literals/u128_literal", (42i64, 0i64));
}

// Sign-extension test: cast u64 (with bit 63 set) → i64 (reinterprets
// as i64::MIN) → i128. The 128-bit high half should be all-ones, since
// the source is signed and negative.
#[test]
fn i128_sign_extend_returns_i64_min() {
    expect_answer("lang/int_literals/i128_sign_extend", (i64::MIN, -1i64));
}

#[test]
fn int_inference_returns_7() {
    expect_answer("lang/int_literals/int_inference", 7i32);
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
fn integer_literal_on_non_num_type_is_rejected() {
    let err = compile_source(
        "struct NotNum { x: u32 }\n\
         fn f() -> u32 { let n: NotNum = 5; 0 }",
    );
    assert!(
        err.contains("expected `NotNum`, got integer"),
        "expected literal-non-Num rejection, got: {}",
        err
    );
}

#[test]
fn integer_literal_in_unbounded_generic_is_rejected() {
    let err = compile_source(
        "fn make<T>() -> T { 42 }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("expected `T`, got integer"),
        "expected literal-unbounded-T rejection, got: {}",
        err
    );
}
