// `?` operator: postfix on `Result<T, E>` expressions inside a
// function returning `Result<U, E>`. On Ok: extract payload. On Err:
// return early with `Err(e)`. NOT desugared early; codegen lowers
// directly so error spans point at the `?` site.

use super::*;

#[test]
fn try_op_ok_chain_returns_42() {
    expect_answer("lang/try_op/ok_chain", 42u32);
}

#[test]
fn try_op_err_propagate_returns_42() {
    expect_answer("lang/try_op/err_propagate", 42u32);
}

#[test]
fn try_op_nested_try_returns_42() {
    expect_answer("lang/try_op/nested_try", 42u32);
}

// Negative: `?` on a non-Result value. Error must point at the `?`
// site, not at synthetic match arms.
#[test]
fn try_op_on_non_result_is_rejected() {
    let err = compile_source(
        "fn f() -> Result<u32, u32> { let x: u32 = 7; let _y = x?; Result::Ok(0) }\n\
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("`?`") && err.contains("Result"),
        "expected `?`-on-non-Result error, got: {}",
        err
    );
}

// Negative: `?` in a function whose return type isn't Result.
#[test]
fn try_op_in_non_result_fn_is_rejected() {
    let err = compile_source(
        "fn ok() -> Result<u32, u32> { Result::Ok(42) }\n\
         fn f() -> u32 { let v = ok()?; v }\n\
         fn answer() -> u32 { f() }",
    );
    assert!(
        err.contains("`?`") && err.contains("Result"),
        "expected `?`-in-non-Result-fn error, got: {}",
        err
    );
}

// Negative: error type mismatch — `?` on `Result<_, u32>` inside a
// function returning `Result<_, u64>`.
#[test]
fn try_op_err_type_mismatch_is_rejected() {
    let err = compile_source(
        "fn ok() -> Result<u32, u32> { Result::Ok(42) }\n\
         fn f() -> Result<u32, u64> { let v = ok()?; Result::Ok(v) }\n\
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("`?`") && err.contains("error type"),
        "expected `?`-err-mismatch error, got: {}",
        err
    );
}
