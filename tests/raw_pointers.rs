//! Checklist for the raw-pointer / `unsafe` rollout.
//!
//! All tests in this file are `#[ignore]`'d until the real-pointer codegen
//! and `*const`/`*mut`/`unsafe` features land. Treat the file as a roadmap;
//! flip `#[ignore]` off as each piece comes online. To run them anyway:
//!
//!     cargo test --test raw_pointers -- --ignored
//!
//! Coverage (in order):
//!   1. Real-pointer codegen for `&`/`&mut` — observable behaviors that fail
//!      under today's by-value/aliased-locals scheme.
//!   2. Raw pointers (`*const T`, `*mut T`) + `unsafe` blocks — the cases the
//!      ref types can't express (refs in struct fields, refs as return types,
//!      recursive types).
//!   3. `safeck.rs` rejections — unsafe operations outside `unsafe` blocks.

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
        prelude: true,
    }
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

fn compile_inline(source: &str) -> Vec<u8> {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, "lib.rs").expect("compile failed");
    module.encode()
}

fn compile_inline_err(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
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

fn answer_u32(bytes: &[u8]) -> i32 {
    let (mut store, instance) = instantiate(bytes);
    let f = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    f.call(&mut store, ()).expect("call failed")
}

// ============================================================================
// 1. Real-pointer codegen for `&` / `&mut`
// ============================================================================

#[test]

fn explicit_deref_through_shared_ref_returns_5() {
    // `*r` where `r: &u32` is *not* unsafe — it's autoderef written explicitly.
    // Today's parser doesn't have a unary `*` operator at all.
    let bytes = compile_inline(
        "fn answer() -> u32 { let x: u32 = 5; let r: &u32 = &x; *r }",
    );
    assert_eq!(answer_u32(&bytes), 5);
}

#[test]

fn explicit_deref_through_mut_ref_writes_back() {
    // Whole-place assignment via `*r = …;` (vs. today's field-only).
    let bytes = compile_inline(
        "fn answer() -> u32 { let mut x: u32 = 1; let r: &mut u32 = &mut x; *r = 42; x }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

#[test]

fn three_mut_calls_in_sequence_returns_3() {
    // Today's out-param scheme already handles this — keep it as a regression
    // check that the new codegen doesn't regress on repeated `&mut` borrows.
    let bytes = compile_inline(
        "struct Counter { n: u32 } \
         fn set(c: &mut Counter, v: u32) -> u32 { c.n = v; c.n } \
         fn answer() -> u32 { \
             let mut c = Counter { n: 0 }; \
             let _a = set(&mut c, 1); \
             let _b = set(&mut c, 2); \
             let _z = set(&mut c, 3); \
             c.n \
         }",
    );
    assert_eq!(answer_u32(&bytes), 3);
}

// ============================================================================
// 2. Raw pointers + `unsafe`
// ============================================================================

#[test]

fn deref_const_pointer_returns_42() {
    let bytes = compile_inline(
        "fn answer() -> u32 { \
             let x: u32 = 42; \
             let p: *const u32 = &x as *const u32; \
             unsafe { *p } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

#[test]

fn write_through_mut_pointer_returns_99() {
    let bytes = compile_inline(
        "fn answer() -> u32 { \
             let mut x: u32 = 0; \
             let p: *mut u32 = &mut x as *mut u32; \
             unsafe { *p = 99; } \
             x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 99);
}

#[test]

fn pointer_field_access_returns_7() {
    let bytes = compile_inline(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let pt = Point { x: 7, y: 14 }; \
             let p: *const Point = &pt as *const Point; \
             unsafe { (*p).x } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

#[test]

fn pointer_field_write_returns_99() {
    let bytes = compile_inline(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let mut pt = Point { x: 1, y: 2 }; \
             let p: *mut Point = &mut pt as *mut Point; \
             unsafe { (*p).x = 99; } \
             pt.x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 99);
}

#[test]

fn returning_a_raw_pointer_writes_back() {
    let bytes = compile_inline(
        "fn through(p: *mut u32) -> *mut u32 { p } \
         fn answer() -> u32 { \
             let mut x: u32 = 1; \
             let p = through(&mut x as *mut u32); \
             unsafe { *p = 42; } \
             x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

#[test]

fn pointer_in_struct_field_returns_30() {
    let bytes = compile_inline(
        "struct Node { value: u32, next: *const Node } \
         fn answer() -> u32 { \
             let t = Node { value: 30, next: 0 as *const Node }; \
             let h = Node { value: 10, next: &t as *const Node }; \
             unsafe { (*h.next).value } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 30);
}

#[test]

fn linked_list_walk_returns_30() {
    // n1 -> n2 -> n3 -> null. Walk to the third node and read its value.
    let bytes = compile_inline(
        "struct Node { value: u32, next: *const Node } \
         fn answer() -> u32 { \
             let n3 = Node { value: 30, next: 0 as *const Node }; \
             let n2 = Node { value: 20, next: &n3 as *const Node }; \
             let n1 = Node { value: 10, next: &n2 as *const Node }; \
             unsafe { (*(*n1.next).next).value } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 30);
}

#[test]

fn raw_pointer_round_trip_through_function() {
    let bytes = compile_inline(
        "fn make(p: *mut u32) -> *mut u32 { p } \
         fn answer() -> u32 { \
             let mut x: u32 = 0; \
             let q: *mut u32 = make(&mut x as *mut u32); \
             unsafe { *q = 7; } \
             unsafe { *q = 8; } \
             x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 8);
}

// ============================================================================
// 3. `safeck.rs` rejections
// ============================================================================

#[test]

fn deref_raw_outside_unsafe_rejected() {
    let err = compile_inline_err(
        "fn answer() -> u32 { \
             let x: u32 = 1; \
             let p: *const u32 = &x as *const u32; \
             *p \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]

fn write_through_raw_outside_unsafe_rejected() {
    let err = compile_inline_err(
        "fn answer() -> u32 { \
             let mut x: u32 = 1; \
             let p: *mut u32 = &mut x as *mut u32; \
             *p = 99; \
             x \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]

fn raw_pointer_field_access_outside_unsafe_rejected() {
    let err = compile_inline_err(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let p: *const Point = &pt as *const Point; \
             (*p).x \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]

fn raw_pointer_field_write_outside_unsafe_rejected() {
    let err = compile_inline_err(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let mut pt = Point { x: 1, y: 2 }; \
             let p: *mut Point = &mut pt as *mut Point; \
             (*p).x = 99; \
             pt.x \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]

fn unsafe_does_not_extend_outside_block() {
    // Compute the deref inside `unsafe`, then attempt a second deref in the
    // outer scope — that one must fail.
    let err = compile_inline_err(
        "fn answer() -> u32 { \
             let x: u32 = 1; \
             let p: *const u32 = &x as *const u32; \
             let _v = unsafe { *p }; \
             *p \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}
