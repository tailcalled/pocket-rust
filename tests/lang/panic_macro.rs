// `panic!(msg: &str)` macro. Lowers to a call to the host-imported
// `env.panic(ptr, len)` function followed by `unreachable`. Diverges
// (type `!`).

use super::*;

// `panic!` on the cold path doesn't fire — function returns 42.
#[test]
fn panic_no_op_path_returns_42() {
    expect_answer("lang/panic_macro/panic_no_op_path", 42u32);
}

// `panic!` actually fires — host stub reads the message bytes out of
// the wasm module's exported `"memory"` and surfaces them in the
// trap. The test asserts the exact custom message text.
#[test]
fn panic_fires_with_message() {
    let err = expect_panic("lang/panic_macro/panic_fires");
    assert!(
        err.contains("custom message at line 5"),
        "expected custom panic message, got: {}",
        err
    );
}

// Negative: `panic!` with no args.
#[test]
fn panic_no_args_is_rejected() {
    let err = compile_source("fn answer() -> u32 { panic!() }");
    assert!(
        err.contains("panic") && err.contains("argument"),
        "expected panic-arity error, got: {}",
        err
    );
}

// Negative: `panic!` with a non-&str arg.
#[test]
fn panic_wrong_arg_type_is_rejected() {
    let err = compile_source("fn answer() -> u32 { panic!(42) }");
    assert!(
        err.contains("type mismatch") || err.contains("&str"),
        "expected panic-arg-type error, got: {}",
        err
    );
}

// Negative: unknown macro name.
#[test]
fn unknown_macro_is_rejected() {
    let err = compile_source("fn answer() -> u32 { foo!(1) }");
    assert!(
        err.contains("unknown macro"),
        "expected unknown-macro error, got: {}",
        err
    );
}

// `panic!` in an `if` arm whose other arm yields a u32 — the if's
// type is u32 (panic's `!` coerces freely).
#[test]
fn panic_in_if_arm_typechecks() {
    use pocket_rust::{Library, Vfs, compile};
    let mut vfs = Vfs::new();
    vfs.insert(
        "lib.rs".to_string(),
        "fn pick(b: bool) -> u32 { if b { 42 } else { panic!(\"no\") } }\n\
         fn answer() -> u32 { pick(true) }"
            .to_string(),
    );
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, "lib.rs").expect("compile failed");
    let bytes = module.encode();
    let (mut store, instance) = instantiate(&bytes);
    let f = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export not found");
    let actual = f.call(&mut store, ()).expect("call failed");
    assert_eq!(actual, 42);
}
