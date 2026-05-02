// Integration tests for pocket-rust's in-language standard library
// (`lib/std/`). Each submodule covers one std type/trait:
//
// - `copy`  — `std::marker::Copy`: primitive impls, generic Copy bounds,
//             user-struct `impl Copy`, Drop/Copy mutual exclusion.
// - `drop`  — `std::ops::Drop`: destructor calls at scope-end, conditional
//             drops with flags, partial-move-of-Drop rejection.
// - `num`   — `std::ops::Num` and the literal-dispatch through `from_i64`,
//             plus operator desugar for `+ - * / %`.
// - `cmp`   — `std::cmp::PartialEq` / `Eq` / `PartialOrd` / `Ord`: operator
//             desugar for `== != < <= > >=`, supertrait dispatch.
//
// Tests of the language features these traits are built on (operator
// parsing, trait dispatch internals, etc.) live in `tests/lang.rs`.

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

pub fn compile_example(dir: &str, entry: &str) -> Vec<u8> {
    let dir_path = format!("examples/{}", dir);
    let root = Path::new(&dir_path);
    let mut vfs = Vfs::new();
    load_dir(root, root, &mut vfs);
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, entry).expect("compile failed");
    module.encode()
}

pub fn compile_source(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
}

pub fn instantiate(bytes: &[u8]) -> (Store<()>, wasmi::Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("wasmi rejected the module");
    let mut store = Store::new(&engine, ());
    let mut linker = <Linker<()>>::new(&engine);
    use wasmi::{Caller, Func};
    let panic_fn = Func::wrap(
        &mut store,
        |mut caller: Caller<'_, ()>, ptr: i32, len: i32| -> Result<(), wasmi::Error> {
            let msg = read_memory_str(&mut caller, ptr as u32, len as u32)
                .unwrap_or_else(|| "<unreadable>".to_string());
            Err(wasmi::Error::new(format!("panic: {}", msg)))
        },
    );
    linker
        .define("env", "panic", panic_fn)
        .expect("define env.panic");
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .expect("instantiate failed");
    (store, instance)
}

pub fn read_memory_str(
    caller: &mut wasmi::Caller<'_, ()>,
    ptr: u32,
    len: u32,
) -> Option<String> {
    let mem = caller.get_export("memory")?.into_memory()?;
    let data = mem.data(&caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    if end > data.len() {
        return None;
    }
    std::str::from_utf8(&data[start..end]).ok().map(String::from)
}

// Run an example expecting a panic; return the trap's formatted
// error string for substring assertion.
pub fn expect_panic(dir: &str) -> String {
    let bytes = compile_example(dir, "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let f = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer: i32` not found");
    let err = f
        .call(&mut store, ())
        .expect_err("expected wasm trap from panic");
    format!("{}", err)
}

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

mod cmp;
mod copy;
mod drop;
mod indexing;
mod num;
mod option;
mod pointer;
mod result;
mod vec;
