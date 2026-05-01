// Top-level functions, calls, and line/column error reporting.

use super::*;
use wasmi::{Engine, Module};

#[test]
fn empty_lib_compiles_to_loadable_wasm() {
    let bytes = compile_example("lang/basics/empty", "lib.rs");
    let engine = Engine::default();
    Module::new(&engine, &bytes[..]).expect("wasmi rejected the module");
}

#[test]
fn answer_returns_42() {
    expect_answer("lang/basics/answer", 42i32);
}

#[test]
fn cross_module_call_returns_42() {
    expect_answer("lang/basics/cross_module", 42i32);
}

#[test]
fn nested_calls_returns_300() {
    expect_answer("lang/basics/nested_calls", 300i32);
}

#[test]
fn return_type_mismatch() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn make() -> Point { Point { x: 1, y: 2 } }\nfn f() -> usize { make() }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected return-type mismatch, got: {}",
        err
    );
}

#[test]
fn arity_mismatch_reports_call_site() {
    let err = compile_source(
        "fn id(x: usize) -> usize { x }\nfn caller() -> usize { id(1, 2) }",
    );
    assert!(
        err.starts_with("lib.rs:2:24:"),
        "expected `lib.rs:2:24:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("expected 1, got 2"),
        "expected arity mismatch detail, got: {}",
        err
    );
}

#[test]
fn unresolved_call_reports_call_site() {
    let err = compile_source("fn main() -> usize { ghost::missing() }");
    assert!(
        err.starts_with("lib.rs:1:22:"),
        "expected `lib.rs:1:22:` prefix, got: {}",
        err
    );
}

#[test]
fn unknown_variable_reports_use_site() {
    let err = compile_source("fn f(a: usize) -> usize { b }");
    assert!(
        err.starts_with("lib.rs:1:27:"),
        "expected `lib.rs:1:27:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("unknown variable"),
        "expected message about unknown variable, got: {}",
        err
    );
}

#[test]
fn lex_error_reports_line_and_column() {
    let err = compile_source("fn answer() -> usize { @ }");
    assert!(
        err.starts_with("lib.rs:1:24:"),
        "expected `lib.rs:1:24:` prefix, got: {}",
        err
    );
}

#[test]
fn parse_error_reports_line_and_column() {
    let err = compile_source("fn ok() -> usize { 42 }\nfn bad)\n");
    assert!(
        err.starts_with("lib.rs:2:7:"),
        "expected `lib.rs:2:7:` prefix, got: {}",
        err
    );
}

#[test]
fn codegen_error_reports_line_and_column() {
    let err = compile_source("fn big() -> usize { 99999999999 }");
    assert!(
        err.starts_with("lib.rs:1:21:"),
        "expected `lib.rs:1:21:` prefix, got: {}",
        err
    );
}
