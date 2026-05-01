// Integration tests for pocket-rust **language intrinsics** — features
// built into the compiler (not the in-language standard library). Each
// submodule covers one feature area:
//
// - `basics`         — top-level functions, calls, line-and-column
//                      error reporting.
// - `block_exprs`    — `{ stmts; tail }` block expressions.
// - `borrowck`       — borrow-check conflicts (move-after-move,
//                      borrow-while-borrowed, etc.).
// - `builtins`       — `¤<type>_<op>` arithmetic/cmp builtins, plus
//                      `¤alloc` / `¤free` / `¤cast`.
// - `enums`          — enum declarations, variants, generic enums.
// - `generics`       — generic functions / structs / impls.
// - `if_exprs`       — `if`/`else` value expressions.
// - `int_literals`   — integer-literal type inference.
// - `let_stmts`      — `let` / `let mut` / assignment.
// - `modules`        — `mod`, `use`, `pub use`, visibility.
// - `patterns`       — `match`, `if let`, pattern syntax.
// - `raw_pointers`   — `*const` / `*mut` codegen + `unsafe` rules.
// - `references`     — `&` / `&mut` codegen, lifetimes, NLL.
// - `structs`        — struct decls, fields, methods.
// - `traits`         — trait decls/impls/dispatch/supertraits.
// - `tuples`         — tuple types/values/indexing.
// - `while_loops`    — `while` / `break` / `continue`.
//
// Tests of stdlib types (`Copy` / `Drop` / `Num` / `PartialEq` / etc.)
// live in `tests/std.rs`.

use pocket_rust::{Library, Vfs, compile};
use std::fs;
use std::path::Path;
use wasmi::{Engine, Linker, Module, Store};

pub fn load_stdlib() -> Library {
    let stdlib_path = Path::new("lib/std");
    let mut vfs = Vfs::new();
    load_dir(stdlib_path, stdlib_path, &mut vfs);
    Library {
        name: "std".to_string(),
        vfs,
        entry: "lib.rs".to_string(),
        prelude: true,
    }
}

pub fn load_dir(root: &Path, dir: &Path, vfs: &mut Vfs) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file_type");
        if file_type.is_dir() {
            load_dir(root, &path, vfs);
        } else if file_type.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rs")
        {
            let rel = path.strip_prefix(root).expect("strip_prefix");
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let source = fs::read_to_string(&path).expect("read source");
            vfs.insert(key, source);
        }
    }
}

// Compile an example directory under `examples/` against the stdlib.
pub fn compile_example(dir: &str, entry: &str) -> Vec<u8> {
    let dir_path = format!("examples/{}", dir);
    let root = Path::new(&dir_path);
    let mut vfs = Vfs::new();
    load_dir(root, root, &mut vfs);
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, entry).expect("compile failed");
    module.encode()
}

// Compile a single inline source as `lib.rs`. Returns the encoded
// wasm module bytes on success.
pub fn compile_inline(source: &str) -> Vec<u8> {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, "lib.rs").expect("compile failed");
    module.encode()
}

// Compile a single inline source as `lib.rs`, expecting an error.
// Returns the formatted error string for substring assertions.
pub fn compile_source(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
}

// Compile a multi-file inline VFS (entry = `lib.rs`), expecting error.
pub fn compile_sources(files: &[(&str, &str)]) -> String {
    let mut vfs = Vfs::new();
    for (name, src) in files {
        vfs.insert((*name).to_string(), (*src).to_string());
    }
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
}

pub fn instantiate(bytes: &[u8]) -> (Store<()>, wasmi::Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("wasmi rejected the module");
    let mut store = Store::new(&engine, ());
    let linker = <Linker<()>>::new(&engine);
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .expect("instantiate failed");
    (store, instance)
}

// Compile `examples/<dir>/lib.rs`, instantiate, invoke `<export>`,
// assert the result. Most tests want `<export>` = `"answer"`.
pub fn expect_export<R>(dir: &str, export: &str, expected: R)
where
    R: wasmi::WasmResults + PartialEq + std::fmt::Debug,
{
    let bytes = compile_example(dir, "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let f = instance
        .get_typed_func::<(), R>(&store, export)
        .expect("export not found / wrong signature");
    let actual = f.call(&mut store, ()).expect("call failed");
    assert_eq!(actual, expected);
}

pub fn expect_answer<R>(dir: &str, expected: R)
where
    R: wasmi::WasmResults + PartialEq + std::fmt::Debug,
{
    expect_export(dir, "answer", expected)
}

// Inline-source helpers for the raw-pointer / unsafe / builtins tests
// that don't have an `examples/` directory.
pub fn answer_u32(bytes: &[u8]) -> i32 {
    let (mut store, instance) = instantiate(bytes);
    let f = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer: i32` not found");
    f.call(&mut store, ()).expect("call failed")
}

mod basics;
mod block_exprs;
mod borrowck;
mod builtins;
mod enums;
mod generics;
mod if_exprs;
mod int_literals;
mod let_stmts;
mod modules;
mod patterns;
mod raw_pointers;
mod references;
mod structs;
mod traits;
mod tuples;
mod while_loops;
