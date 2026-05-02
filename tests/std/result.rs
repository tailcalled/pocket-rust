// `std::result::Result<T, E>` — `Ok(T)` / `Err(E)` enum, plus
// `is_ok`, `is_err`, `unwrap_or`, `ok`, `err`, `and`, `or`, `flatten`,
// `transpose` inherent methods. Construction-and-match coverage of
// the same shape lives in `tests/lang/patterns.rs`; this file
// specifically exercises the stdlib type and its methods.

use super::*;

#[test]
fn result_is_ok_returns_42() {
    expect_answer("std/result/is_ok", 42u32);
}

#[test]
fn result_is_err_returns_42() {
    expect_answer("std/result/is_err", 42u32);
}

#[test]
fn result_unwrap_or_ok_returns_42() {
    expect_answer("std/result/unwrap_or_ok", 42u32);
}

#[test]
fn result_unwrap_or_err_returns_42() {
    expect_answer("std/result/unwrap_or_err", 42u32);
}

#[test]
fn result_match_ok_returns_42() {
    expect_answer("std/result/match_ok", 42u32);
}

#[test]
fn result_match_err_returns_42() {
    expect_answer("std/result/match_err", 42u32);
}

#[test]
fn result_ok_returns_42() {
    expect_answer("std/result/ok", 42u32);
}

#[test]
fn result_err_returns_42() {
    expect_answer("std/result/err", 42u32);
}

#[test]
fn result_and_ok_returns_42() {
    expect_answer("std/result/and_ok", 42u32);
}

#[test]
fn result_and_err_returns_42() {
    expect_answer("std/result/and_err", 42u32);
}

#[test]
fn result_or_ok_returns_42() {
    expect_answer("std/result/or_ok", 42u32);
}

#[test]
fn result_or_err_returns_42() {
    expect_answer("std/result/or_err", 42u32);
}

#[test]
fn result_flatten_ok_ok_returns_42() {
    expect_answer("std/result/flatten_ok_ok", 42u32);
}

#[test]
fn result_flatten_ok_err_returns_42() {
    expect_answer("std/result/flatten_ok_err", 42u32);
}

#[test]
fn result_flatten_outer_err_returns_42() {
    expect_answer("std/result/flatten_outer_err", 42u32);
}

#[test]
fn result_transpose_ok_some_returns_42() {
    expect_answer("std/result/transpose_ok_some", 42u32);
}

#[test]
fn result_transpose_ok_none_returns_42() {
    expect_answer("std/result/transpose_ok_none", 42u32);
}

#[test]
fn result_transpose_err_returns_42() {
    expect_answer("std/result/transpose_err", 42u32);
}

// `Result<T, !>::into_ok` — Err arm is uninhabited, exhaustiveness
// skips it, the Ok arm extracts the value.
#[test]
fn result_into_ok_returns_42() {
    expect_answer("std/result/into_ok", 42u32);
}

// `Result<!, E>::into_err` — symmetric: Ok arm is uninhabited.
#[test]
fn result_into_err_returns_42() {
    expect_answer("std/result/into_err", 42u32);
}

// Negative: skipping Err in a regular `match` on `Result<u32, u32>`
// (where E is *not* `!`) is still rejected — uninhabited-skipping
// only fires when the variant's payload is uninhabited.
#[test]
fn result_match_missing_err_arm_inhabited_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let r: Result<u32, u32> = Result::Ok(42); \
             match r { Result::Ok(v) => v, } \
         }",
    );
    assert!(
        err.contains("non-exhaustive match"),
        "expected non-exhaustive-match error, got: {}",
        err
    );
}

// Negative: dispatch sanity — calling `into_ok` on a `Result<T, E>`
// where E isn't `!` finds no matching impl (the `impl<T> Result<T, !>`
// pattern requires E exactly `!`).
#[test]
fn result_into_ok_on_inhabited_err_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let r: Result<u32, u32> = Result::Ok(42); \
             r.into_ok() \
         }",
    );
    assert!(
        err.contains("no method") && err.contains("into_ok"),
        "expected no-method error, got: {}",
        err
    );
}

// Negative: `Result::flatten` is only defined on `Result<Result<T, E>, E>`
// — calling it on a singly-nested `Result<T, E>` (where T isn't a
// Result) hits the second-impl-block constraint and dispatch fails.
#[test]
fn result_flatten_on_non_nested_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let r: Result<u32, u32> = Result::Ok(42); \
             r.flatten().unwrap_or(0) \
         }",
    );
    assert!(
        err.contains("no method") && err.contains("flatten"),
        "expected no-flatten-on-flat-Result error, got: {}",
        err
    );
}

// Negative: arity check on `Result` type-args. The trait declares two
// type parameters (`T, E`); writing `Result<u32>` should be rejected.
#[test]
fn result_wrong_arity_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let r: Result<u32> = Result::Ok(42); \
             r.unwrap_or(0) \
         }",
    );
    assert!(
        err.contains("type arguments") || err.contains("expected 2"),
        "expected wrong-arity error, got: {}",
        err
    );
}

// Negative: `transpose` is only defined on `Result<Option<T>, E>`.
// Calling it on a non-Option-payload Result fails to find the method.
#[test]
fn result_transpose_on_non_option_payload_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let r: Result<u32, u32> = Result::Ok(42); \
             let _o = r.transpose(); \
             0 \
         }",
    );
    assert!(
        err.contains("no method") && err.contains("transpose"),
        "expected no-transpose-on-non-Option error, got: {}",
        err
    );
}
