// Built-in macros: `panic!` (handled in tests/lang/panic_macro.rs)
// and `vec!` (here). `vec!` desugars at parse time to a block
// expression that calls `Vec::new()` then `.push(...)` for each
// element — so the test exercises both bracket parsing and the
// generated AST.

use super::*;

// `vec![a, b, c]` — three elements, type inferred from contents.
#[test]
fn vec_basic_returns_42() {
    expect_answer("lang/macros/vec_basic", 42i32);
}

// `vec![]` — empty form. Element type comes from the let-binding's
// `Vec<u32>` annotation rather than from the contents.
#[test]
fn vec_empty_returns_42() {
    expect_answer("lang/macros/vec_empty", 42i32);
}

// `vec![value; count]` — repeat form. Builds a Vec of `count`
// clones of `value`. `T: Clone` is required (Copy types qualify).
#[test]
fn vec_repeat_returns_42() {
    expect_answer("lang/macros/vec_repeat", 42u32);
}

// Repeat form with the count taken from a local — the count
// expression is evaluated once before the loop runs.
#[test]
fn vec_repeat_dynamic_returns_42() {
    expect_answer("lang/macros/vec_repeat_dynamic", 42u32);
}

// `matches!(scrut, pattern)` — desugars to `match scrut { pattern
// => true, _ => false }`. Exercises the basic pattern-match.
#[test]
fn matches_basic_returns_42() {
    expect_answer("lang/macros/matches_basic", 42i32);
}

// `matches!(scrut, pattern if guard)` — the optional `if guard`
// runs after the pattern matches; the pattern's bindings are in
// scope inside the guard.
#[test]
fn matches_with_guard_returns_42() {
    expect_answer("lang/macros/matches_with_guard", 42i32);
}

// `matches!` returns false when the scrutinee doesn't match the
// pattern (the wildcard arm fires).
#[test]
fn matches_no_match_returns_42() {
    expect_answer("lang/macros/matches_no_match", 42i32);
}

// Negative: `matches!` requires the args to be `(scrut, pattern)`
// — using just `(scrut)` (one expression, no pattern) errors with
// the parser's "`,`"-expected message at the parens close.
#[test]
fn matches_missing_pattern_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: u32 = 1; if matches!(x) { 0 } else { 42 } }",
    );
    assert!(
        err.contains("`,`") || err.contains("expected"),
        "expected parser error on missing pattern, got: {}",
        err
    );
}

// `&&T` in type position must split into `& &T` (the lexer emits
// the `&&` as one `AndAnd` token; the type parser splits it). If
// the split misfired we'd see a "expected type, got `&&`"-style
// parse error here. We assert the snippet typechecks past parsing —
// a more substantive `&&str`-using example is exercised by the
// stdlib's `impl PartialEq for &str` and the str-eq tests.
#[test]
fn ref_to_ref_type_position_parses() {
    let mut vfs = Vfs::new();
    vfs.insert(
        "lib.rs".to_string(),
        "fn id(r: &&u32) -> u32 { **r }\n\
         fn answer() -> u32 { let x: u32 = 42; let r: &u32 = &x; id(&r) }"
            .to_string(),
    );
    let result = pocket_rust::compile(&[load_stdlib()], &vfs, "lib.rs");
    assert!(
        result.is_ok(),
        "expected `&&u32` to parse as `& &u32`, got: {:?}",
        result.err()
    );
}
