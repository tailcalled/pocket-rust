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

#[test]
fn neg_lit_returns_42() {
    // `-1isize + 43isize = 42`. Tests that `-INT_LIT` parses as a
    // single negative literal and pins to the let-annotated type.
    expect_answer("lang/int_literals/neg_lit", 42u32);
}

#[test]
fn neg_arith_returns_42() {
    // Unary minus on a non-literal expression desugars to a method
    // call to `<T as VecSpace>::neg`. `(-50) + 92 = 42`.
    expect_answer("lang/int_literals/neg_arith", 42u32);
}

#[test]
fn neg_lit_unsigned_is_rejected() {
    let err = compile_source("fn f() -> u32 { let n: u32 = -4; n }");
    assert!(
        err.contains("cannot apply unary `-`"),
        "expected unary-minus-on-unsigned error, got: {}",
        err
    );
}

#[test]
fn literal_arith_returns_42() {
    // No let-annotations, no other context — both literals are
    // unbound num-lit vars. Method dispatch on the unbound receiver
    // goes through the implicit `T: Num` bound (Num supertrait
    // closure) to find `add` on `VecSpace`. The result type pins to
    // i32 by default, then casts up via the fn signature.
    expect_answer("lang/int_literals/literal_arith", 42i32);
}

#[test]
fn literal_neg_returns_neg_42() {
    // Unary minus on a non-literal `(30 + 12)` desugars to a method
    // call. The inner addition is itself a method call on unbound
    // num-lit vars; the outer `.neg()` is also dispatched on an
    // unbound num-lit var (the inner add's result var). Both go
    // through Num/VecSpace.
    expect_answer("lang/int_literals/literal_neg", -42i32);
}

#[test]
fn neg_i32_min_returns_min() {
    // `-2147483648` is `i32::MIN`. The magnitude (2147483648) does
    // NOT fit in `i32`'s positive range (max = 2147483647), so this
    // would fail under a `2147483648.neg()` desugar — the literal
    // range check rejects the magnitude before `neg` ever runs.
    // `NegIntLit` carries the sign through inference, letting the
    // range check see `-2147483648` against `i32::MIN..=i32::MAX`.
    expect_answer("lang/int_literals/i32_min", i32::MIN);
}

// `'X'` char literal has type `char`. `as u32` (or any int kind)
// extracts the Unicode codepoint.
#[test]
fn char_lit_basic_returns_42() {
    expect_answer("lang/int_literals/char_lit_basic", 42u32);
}

// Common escape sequences (`\n`, `\t`, `\\`, `\0`).
#[test]
fn char_lit_escapes_returns_42() {
    expect_answer("lang/int_literals/char_lit_escapes", 42u32);
}

// Lexer disambiguation: `'a` is a lifetime, `'\n'` is a char
// literal; both appear in the same source.
#[test]
fn char_lit_disambig_lifetime_returns_42() {
    expect_answer("lang/int_literals/char_lit_disambig_lifetime", 42u32);
}

// UTF-8 multi-byte char literal: '¥' is U+00A5 (165), encoded as
// the two source bytes 0xC2 0xA5; the lexer's UTF-8 decoder
// produces the codepoint.
#[test]
fn char_lit_unicode_returns_42() {
    expect_answer("lang/int_literals/char_lit_unicode", 42u32);
}

// `\u{HH..}` escape — 1-6 hex digits naming a Unicode codepoint.
#[test]
fn char_lit_unicode_escape_returns_42() {
    expect_answer("lang/int_literals/char_lit_unicode_escape", 42u32);
}

// Negative: unterminated char literal after the first char.
#[test]
fn char_lit_unterminated_is_rejected() {
    let err = compile_source("fn answer() -> u32 { 'A' as u32 + 'B as u32 }");
    assert!(
        !err.is_empty(),
        "expected lex error, got: {}",
        err
    );
}

// Negative: unknown char escape.
#[test]
fn char_lit_unknown_escape_is_rejected() {
    let err = compile_source("fn answer() -> u32 { '\\q' as u32 }");
    assert!(
        err.contains("unknown char escape"),
        "expected unknown-escape error, got: {}",
        err
    );
}

// Negative: surrogate codepoint (illegal — reserved for UTF-16
// pairs and never appears in a valid Unicode scalar value).
#[test]
fn char_lit_surrogate_is_rejected() {
    let err = compile_source("fn answer() -> u32 { '\\u{D800}' as u32 }");
    assert!(
        err.contains("invalid Unicode codepoint"),
        "expected surrogate-rejection error, got: {}",
        err
    );
}

// Negative: char literal is type `char`, not an integer. Direct
// arithmetic without `as` fails.
#[test]
fn char_lit_without_cast_is_rejected() {
    let err = compile_source("fn answer() -> u32 { 'A' + 7 }");
    assert!(
        err.contains("char") || err.contains("type mismatch") || err.contains("no method"),
        "expected char-not-int error, got: {}",
        err
    );
}
