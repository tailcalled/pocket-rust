// String literals `"..."`. Lex/parse/typeck/codegen end-to-end.
//
// Positive: the literal compiles, lands in the module's data segment
// at a known offset, surfaces as `&'static str` with the correct
// byte length and decoded content. Codegen interns by payload so
// repeated literals share a slot.
//
// Negative: lexer rejects unterminated strings and unknown escapes;
// typeck rejects passing `&str` where a non-str type is expected.

use super::*;

#[test]
fn str_literal_basic_len_returns_42() {
    expect_answer("lang/str_literal/basic_len", 42u32);
}

#[test]
fn str_literal_empty_returns_42() {
    expect_answer("lang/str_literal/empty", 42u32);
}

#[test]
fn str_literal_escapes_returns_42() {
    expect_answer("lang/str_literal/escapes", 42u32);
}

#[test]
fn str_literal_multi_dedup_returns_42() {
    expect_answer("lang/str_literal/multi_dedup", 42u32);
}

#[test]
fn str_literal_in_struct_field_returns_42() {
    expect_answer("lang/str_literal/in_struct_field", 42u32);
}

#[test]
fn str_literal_in_match_arm_returns_42() {
    expect_answer("lang/str_literal/in_match_arm", 42u32);
}

// ─── Negative tests ──────────────────────────────────────────────

#[test]
fn unterminated_string_literal_is_rejected() {
    let err = compile_source("fn answer() -> u32 { let s: &str = \"hello; 0 }");
    assert!(
        err.contains("unterminated string"),
        "expected unterminated-string error, got: {}",
        err,
    );
}

#[test]
fn unknown_escape_is_rejected() {
    let err = compile_source("fn answer() -> u32 { let s: &str = \"hi\\q\"; 0 }");
    assert!(
        err.contains("unknown escape sequence") && err.contains("q"),
        "expected unknown-escape error, got: {}",
        err,
    );
}

#[test]
fn str_literal_passed_where_int_expected_is_rejected() {
    let err = compile_source(
        "fn count(n: u32) -> u32 { n } \
         fn answer() -> u32 { count(\"hello\") }",
    );
    assert!(
        err.contains("type mismatch"),
        "expected type mismatch when passing str literal where u32 expected, got: {}",
        err,
    );
}

#[test]
fn str_literal_assigned_to_int_let_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let n: u32 = \"hello\"; n }",
    );
    assert!(
        err.contains("type mismatch"),
        "expected type mismatch on let n: u32 = str-literal, got: {}",
        err,
    );
}
