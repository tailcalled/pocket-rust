use pocket_rust::{Library, Vfs, compile};
use std::fs;
use std::path::Path;
use wasmi::{Engine, Linker, Module, Store};

fn load_stdlib() -> Library {
    let stdlib_path = Path::new("lib/std");
    let mut vfs = Vfs::new();
    load_dir(stdlib_path, stdlib_path, &mut vfs);
    Library {
        name: "std".to_string(),
        vfs,
        entry: "lib.rs".to_string(),
    }
}

fn compile_example(dir: &str, entry: &str) -> Vec<u8> {
    let dir_path = format!("examples/{}", dir);
    let root = Path::new(&dir_path);
    let mut vfs = Vfs::new();
    load_dir(root, root, &mut vfs);
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, entry).expect("compile failed");
    module.encode()
}

fn load_dir(root: &Path, dir: &Path, vfs: &mut Vfs) {
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

fn instantiate(bytes: &[u8]) -> (Store<()>, wasmi::Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("wasmi rejected the module");
    let mut store = Store::new(&engine, ());
    let linker = <Linker<()>>::new(&engine);
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .expect("instantiate failed");
    (store, instance)
}

#[test]
fn empty_lib_compiles_to_loadable_wasm() {
    let bytes = compile_example("empty", "lib.rs");
    let engine = Engine::default();
    Module::new(&engine, &bytes[..]).expect("wasmi rejected the module");
}

#[test]
fn answer_returns_42() {
    let bytes = compile_example("answer", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

#[test]
fn cross_module_call_returns_42() {
    let bytes = compile_example("cross_module", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

#[test]
fn nested_calls_returns_300() {
    let bytes = compile_example("nested_calls", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 300);
}

#[test]
fn structs_returns_40() {
    let bytes = compile_example("structs", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 40);
}

#[test]
fn borrows_returns_40() {
    let bytes = compile_example("borrows", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 40);
}

#[test]
fn uses_std_dummy_id_returns_7() {
    let bytes = compile_example("uses_std", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

#[test]
fn lets_returns_5() {
    let bytes = compile_example("lets", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 5);
}

#[test]
fn block_expr_returns_11() {
    let bytes = compile_example("block_expr", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 11);
}

#[test]
fn escaping_borrow_returns_42() {
    let bytes = compile_example("escaping_borrow", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// KNOWN-FAILING: pocket-rust's borrowck is stricter than Rust here.
//
// `examples/inner_borrow_lifetime/lib.rs` is valid Rust — the borrow `&pt1`
// inside the inner block is bound to `r`, which goes out of scope at the
// inner `}`. The block's tail is `r.x` (a `usize`, copied through the
// reference), so nothing borrowing `pt1` escapes, and `let q = pt1;` is
// accepted. `q.x` is `5`.
//
// Pocket-rust currently keeps the `&pt1` borrow alive for the rest of the
// function (borrows are scoped per-`Call`, not per-binding), so it rejects
// the `let q = pt1;` move and `compile_example` panics. This test will
// start passing the day we add per-binding borrow lifetimes.
#[test]
fn u8_literal_returns_200() {
    let bytes = compile_example("u8_literal", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 200);
}

#[test]
fn i64_literal_returns_9_000_000_000() {
    let bytes = compile_example("i64_literal", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i64>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 9_000_000_000);
}

#[test]
fn let_mut_scalar_returns_99() {
    let bytes = compile_example("let_mut_scalar", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 99);
}

#[test]
fn let_mut_record_returns_99() {
    let bytes = compile_example("let_mut_record", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 99);
}

#[test]
fn let_mut_nested_returns_99() {
    let bytes = compile_example("let_mut_nested", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 99);
}

#[test]
fn int_inference_returns_7() {
    let bytes = compile_example("int_inference", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

#[test]
fn mut_ref_through_binding_returns_99() {
    let bytes = compile_example("mut_ref_through_binding", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 99);
}

#[test]
fn mut_ref_direct_returns_50() {
    let bytes = compile_example("mut_ref_direct", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 50);
}

#[test]
fn mut_ref_field_returns_77() {
    let bytes = compile_example("mut_ref_field", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 77);
}

#[test]
fn inner_borrow_lifetime_returns_5() {
    let bytes = compile_example("inner_borrow_lifetime", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 5);
}

#[test]
fn borrow_field_returns_42() {
    let bytes = compile_example("borrow_field", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

#[test]
fn methods_returns_42() {
    let bytes = compile_example("methods", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

#[test]
fn generic_id_returns_100() {
    let bytes = compile_example("generic_id", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 100);
}

#[test]
fn generic_pair_returns_7() {
    let bytes = compile_example("generic_pair", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

#[test]
fn uses_std_generic_struct_returns_42() {
    let bytes = compile_example("uses_std_generic_struct", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

#[test]
fn uses_std_generic_returns_42() {
    let bytes = compile_example("uses_std_generic", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}
