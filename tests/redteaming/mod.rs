// Shared helpers for red-team tests. Mirrors `tests/lang/mod.rs` but
// trimmed to what the rt suites actually use (compile + assert error
// or compile + run + assert export).

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

pub fn try_compile_example(dir: &str, entry: &str) -> Result<Vec<u8>, String> {
    let dir_path = format!("examples/{}", dir);
    let root = Path::new(&dir_path);
    let mut vfs = Vfs::new();
    load_dir(root, root, &mut vfs);
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, entry).map(|m| m.encode())
}

pub fn compile_source(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
}

pub fn try_compile_source(source: &str) -> Result<Vec<u8>, String> {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").map(|m| m.encode())
}

pub fn instantiate(bytes: &[u8]) -> (Store<()>, wasmi::Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("wasmi rejected the module");
    let mut store = Store::new(&engine, ());
    let mut linker = <Linker<()>>::new(&engine);
    use wasmi::{Caller, Func};
    let panic_fn = Func::wrap(
        &mut store,
        |_caller: Caller<'_, ()>, _ptr: i32, _len: i32| -> Result<(), wasmi::Error> {
            Err(wasmi::Error::new("panic"))
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

pub fn expect_answer<R>(dir: &str, expected: R)
where
    R: wasmi::WasmResults + PartialEq + std::fmt::Debug,
{
    let bytes = compile_example(dir, "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let f = instance
        .get_typed_func::<(), R>(&store, "answer")
        .expect("export `answer` not found");
    let actual = f.call(&mut store, ()).expect("call failed");
    assert_eq!(actual, expected);
}

mod rt1;
mod rt2;
mod rt3;
mod rt4;
mod rt5;
mod rt6;
