// `str` fat-ref ABI: `&str` is layout-identical to `&[u8]` — 2 i32s
// (data ptr + length). These tests mirror the slice ABI tests and
// prove the same value-positions work for str refs.
//
// Construction goes through `¤make_str` (raw-parts route) since
// string literals aren't landed yet. Most tests build a 5-byte
// "hello" via inline byte writes through `byte_add`. Observation is
// `s.len()` (and `s.as_bytes().len()` for the as_bytes path).

use super::*;

#[test]
fn str_through_fn_arg_returns_42() {
    expect_answer("lang/str/through_fn", 42u32);
}

#[test]
fn str_returned_from_fn_returns_42() {
    expect_answer("lang/str/returned_from_fn", 42u32);
}

#[test]
fn str_as_bytes_len_returns_42() {
    expect_answer("lang/str/as_bytes_len", 42u32);
}

#[test]
fn str_in_struct_field_returns_42() {
    expect_answer("lang/str/in_struct_field", 42u32);
}

#[test]
fn str_in_tuple_returns_42() {
    expect_answer("lang/str/in_tuple", 42u32);
}

#[test]
fn str_in_enum_variant_returns_42() {
    expect_answer("lang/str/in_enum_variant", 42u32);
}

#[test]
fn str_if_result_returns_42() {
    expect_answer("lang/str/if_result", 42u32);
}

#[test]
fn str_match_result_returns_42() {
    expect_answer("lang/str/match_result", 42u32);
}

#[test]
fn str_through_generic_returns_42() {
    expect_answer("lang/str/through_generic", 42u32);
}

#[test]
fn str_is_copy_returns_42() {
    expect_answer("lang/str/is_copy", 42u32);
}

#[test]
fn str_spilled_binding_returns_42() {
    expect_answer("lang/str/spilled_binding", 42u32);
}

#[test]
fn vec_of_strs_returns_42() {
    expect_answer("lang/str/vec_of_strs", 42u32);
}

#[test]
fn str_in_struct_by_value_returns_42() {
    expect_answer("lang/str/struct_by_value", 42u32);
}

#[test]
fn str_in_option_payload_returns_42() {
    expect_answer("lang/str/in_option_payload", 42u32);
}

#[test]
fn str_composite_chain_returns_42() {
    expect_answer("lang/str/composite_chain", 42u32);
}

// ─── Negative tests ──────────────────────────────────────────────

#[test]
fn passing_str_where_slice_u8_expected_is_rejected() {
    // `&str` and `&[u8]` are layout-identical but typecheck as
    // distinct types — the conversion goes through `as_bytes`.
    // Passing `&str` directly where `&[u8]` is expected must error.
    let err = compile_source(
        "fn count(s: &[u8]) -> u32 { s.len() as u32 } \
         fn answer() -> u32 { \
             let p: *mut u8 = unsafe { ¤alloc(1) }; \
             let s: &str = unsafe { ¤make_str(p as *const u8, 0) }; \
             count(s) \
         }",
    );
    assert!(
        err.contains("type mismatch") || (err.contains("str") && err.contains("[u8]")),
        "expected &str-vs-&[u8] mismatch, got: {}",
        err,
    );
}

#[test]
fn passing_slice_u8_where_str_expected_is_rejected() {
    let err = compile_source(
        "fn count(s: &str) -> u32 { s.len() as u32 } \
         fn answer() -> u32 { \
             let mut v: Vec<u8> = Vec::new(); \
             v.push(0); \
             count(v.as_slice()) \
         }",
    );
    assert!(
        err.contains("type mismatch") || (err.contains("str") && err.contains("[u8]")),
        "expected &[u8]-vs-&str mismatch, got: {}",
        err,
    );
}

#[test]
fn make_str_with_wrong_arg_count_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let s: &str = unsafe { ¤make_str() }; \
             s.len() as u32 \
         }",
    );
    assert!(
        err.contains("¤make_str") && err.contains("argument"),
        "expected make_str arity error, got: {}",
        err,
    );
}

#[test]
fn make_str_with_type_arg_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let p: *mut u8 = unsafe { ¤alloc(0) }; \
             let s: &str = unsafe { ¤make_str::<u8>(p as *const u8, 0) }; \
             s.len() as u32 \
         }",
    );
    assert!(
        err.contains("¤make_str") && err.contains("type argument"),
        "expected make_str type-arg rejection, got: {}",
        err,
    );
}

#[test]
fn str_len_intrinsic_on_non_str_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(1); \
             ¤str_len(v.as_slice()) as u32 \
         }",
    );
    // `&[u32]` is not `&str`.
    assert!(
        err.contains("type mismatch") || err.contains("str"),
        "expected str_len type mismatch, got: {}",
        err,
    );
}

#[test]
fn undeclared_lifetime_other_than_static_still_rejected() {
    // `'static` is the only built-in lifetime; arbitrary names must
    // still be declared in the enclosing fn's `<'a, …>` params.
    let err = compile_source(
        "fn f(s: &'a str) -> u32 { s.len() as u32 } \
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("undeclared lifetime") && err.contains("'a"),
        "expected undeclared-lifetime error for `'a`, got: {}",
        err,
    );
}
