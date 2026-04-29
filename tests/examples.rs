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
fn copy_double_use_returns_7() {
    let bytes = compile_example("copy_double_use", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

#[test]
fn place_borrow_noncopy_field_returns_7() {
    let bytes = compile_example("place_borrow_noncopy_field", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

#[test]
fn place_borrow_through_ref_returns_42() {
    let bytes = compile_example("place_borrow_through_ref", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

#[test]
fn nll_sequential_borrows_returns_7() {
    let bytes = compile_example("nll_sequential_borrows", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

#[test]
fn nll_borrow_then_move_returns_7() {
    let bytes = compile_example("nll_borrow_then_move", "lib.rs");
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

// Named lifetimes on functions tie param to return type. `pick_first<'a>`
// picks `x`'s lifetime; the elided y arg gets a fresh inferred one and
// doesn't constrain the result.
#[test]
fn lifetime_named_returns_42() {
    let bytes = compile_example("lifetime_named", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Refs in struct fields: a generic `Wrapper<'a>` holds `&'a Inner` and a
// field-access produces the held borrow.
#[test]
fn lifetime_struct_field_returns_42() {
    let bytes = compile_example("lifetime_struct_field", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// `&'a self` receiver tied to the impl's lifetime param routes the
// receiver's borrow into the return ref.
#[test]
fn lifetime_self_receiver_returns_42() {
    let bytes = compile_example("lifetime_self_receiver", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Partial-concrete impl: `impl<T> Pair<usize, T>` matches `Pair<u32, T>`
// for any T's substitution. Method dispatches via try_match on impl_target.
#[test]
fn impl_partial_concrete_returns_42() {
    let bytes = compile_example("impl_partial_concrete", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Repeat-param impl: `impl<T> Pair<T, T>` only matches when both type
// args coincide. Matching binds T once and unifies the second occurrence.
#[test]
fn impl_repeat_param_returns_42() {
    let bytes = compile_example("impl_repeat_param", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Fully-concrete impl: zero impl type-params, target is concrete.
#[test]
fn impl_fully_concrete_returns_42() {
    let bytes = compile_example("impl_fully_concrete", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T4.5: drops at block-expression scope end. The inner block has a
// tail (`42`) and a Drop binding (`_l`). Codegen saves the tail value
// to a local, runs `_l`'s drop (writes 7 to c), reloads the tail, then
// the outer fn reads `c`.
#[test]
fn drop_block_expr_returns_7() {
    let bytes = compile_example("drop_block_expr", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 7);
}

// T4.5: Drop function parameters. `take(l: Logger)` drops `l` at fn
// end (after returning 42). The outer reads `c` after `take` returns
// and observes the drop side effect.
#[test]
fn drop_fn_param_returns_1() {
    let bytes = compile_example("drop_fn_param", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 1);
}

// T2.5b: trait methods with their own type-params. `Pick::pick<U>`
// declares a method-level type-param. The impl on `First` carries a
// matching `<U>` (validated α-equivalently). At a symbolic call
// `t.pick::<u32>(11, 22)` through a `T: Pick` bound, codegen monomorphizes
// `First::pick<u32>` — the literal args land on `u32` (via Num), and
// the receiver `First` lands on the concrete impl after `solve_impl`.
#[test]
fn trait_method_generic_returns_11() {
    let bytes = compile_example("trait_method_generic", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 11);
}

// Lifetime cleanup: nested per-slot field borrow tracking. Reading
// `o.i.r` follows a multi-segment field path through a struct
// containing a struct-with-ref. Borrowck records the inner borrow at
// the outer holder under the nested path `["i", "r"]`, and reads
// through that path return the borrow correctly. The runtime side
// just verifies the value flows through.
#[test]
fn nested_field_borrow_returns_42() {
    let bytes = compile_example("nested_field_borrow", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Lifetime cleanup: anonymous `'_` lifetime. `'_` parses to a fresh
// `Inferred(0)` placeholder per occurrence (rather than a regular
// `Named("_")`), so it works in let-binding annotations and impl
// targets without users having to invent a unique `'a` name. Here
// `impl Drop for Logger<'_>` and `let _l: Logger<'_> = ...` both rely
// on it.
#[test]
fn anon_lifetime_impl_returns_42() {
    let bytes = compile_example("anon_lifetime_impl", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T4.6: move-aware drops. `let _y: Logger = l;` is a whole-binding
// move of a Drop value — borrowck records it, codegen skips `l`'s
// implicit scope-end drop, and only `_y`'s drop fires. The Logger
// drop body sets `*sink = *counter; *counter = 1;`. With one drop:
// sink := 0 (the original c), counter := 1. With two drops we'd see
// sink := 1 from the second pass. The function returns `s`, so the
// expected result is 0.
#[test]
fn drop_moved_returns_0() {
    let bytes = compile_example("drop_moved", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 0);
}

// T2.6.5: when type-pattern matching yields multiple candidates, drop
// those whose `derive_recv_adjust` would error (e.g. method takes
// `self` by value but recv is a borrow). Here both `impl Show for Foo`
// and `impl<T: Show> Show for &T` type-match `&Foo`, but only the
// blanket can adjust — the inherent would move out of borrow.
#[test]
fn dispatch_adjust_filter_returns_99() {
    let bytes = compile_example("dispatch_adjust_filter", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 99);
}

// T2.6: concrete trait dispatch on a primitive recv. `x.show()` for
// `x: u32` finds `impl Show for u32` even though the impl_target isn't
// a struct path (so the old `find_method_candidates` filter would have
// missed it).
#[test]
fn trait_impl_on_u32_returns_42() {
    let bytes = compile_example("trait_impl_on_u32", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T2.6: blanket impl `impl<T> Show for &T` dispatches when the recv is
// a `&Foo` and Foo doesn't otherwise implement Show. Verifies the
// non-struct target path through method-call dispatch.
#[test]
fn trait_blanket_on_ref_returns_42() {
    let bytes = compile_example("trait_blanket_on_ref", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T5.5: integer literal lands on a user type via `impl Num for Wrap`.
// `let w: Wrap = 42` resolves the literal to `Wrap` (instead of erroring
// because Wrap isn't an integer kind), and codegen routes through
// `<Wrap as Num>::from_i64`.
#[test]
fn num_user_type_returns_42() {
    let bytes = compile_example("num_user_type", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T5.5: integer literal in a `<T: Num>` generic body. Inside `make<T:
// Num>() -> T { 42 }`, the literal lands on `T` (Param-typed); at mono
// time `T` resolves to `u32`, and the literal codegens as
// `<u32 as Num>::from_i64(42)`.
#[test]
fn num_generic_body_returns_42() {
    let bytes = compile_example("num_generic_body", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T5: every integer literal desugars to `<T as Num>::from_i64(value)`.
// This test exercises u8, i64, and u32 literal codegen end-to-end —
// each literal becomes a real call to the relevant `from_i64` impl
// (no inlining), and the values flow through to the answer.
#[test]
fn num_literal_dispatch_returns_42() {
    let bytes = compile_example("num_literal_dispatch", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T4: dropping a Logger at the inner block's scope end writes 1 to a
// shared counter via `*self.counter = 1`. The outer fn reads `c` after
// the block, observing the drop side effect.
#[test]
fn drop_logger_returns_1() {
    let bytes = compile_example("drop_logger", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 1);
}

// T2.5: trait dispatch through `&self` autoref'ing an owned generic
// receiver. `t.get()` inside `fn use_get<T: Get>(t: T)` where Get takes
// `&self` must autoref `t` before the trait call.
#[test]
fn trait_borrow_self_dispatch_returns_42() {
    let bytes = compile_example("trait_borrow_self_dispatch", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T2.5: `impl<T: Copy> Copy for Wrap<T> {}` validates: the bound makes
// `Param(T)` Copy so the `inner: T` field passes the field-Copy check.
#[test]
fn copy_generic_with_bound_returns_42() {
    let bytes = compile_example("copy_generic_with_bound", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T2.5: in a generic body with `T: Copy`, reading `t` after `let s = t`
// is a value copy (not a move) because the bound makes `Param(T)` Copy.
#[test]
fn copy_param_via_bound_returns_42() {
    let bytes = compile_example("copy_param_via_bound", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T3: user-defined `impl Copy for Pt {}`. Reading `p` after `let q = p`
// should be allowed since Pt is Copy.
#[test]
fn copy_user_struct_returns_42() {
    let bytes = compile_example("copy_user_struct", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T3: `&mut T` is NOT Copy — assigning a mut-ref to another binding
// moves it (preserves exclusivity). Reading the original after move
// would be rejected; this test passes the move via the new binding.
#[test]
fn copy_mut_ref_not_copy_returns_42() {
    let bytes = compile_example("copy_mut_ref_not_copy", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T2: concrete trait method dispatch via `impl Show for Foo` + `f.show()`.
#[test]
fn trait_concrete_dispatch_returns_42() {
    let bytes = compile_example("trait_concrete_dispatch", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T2: recursive impl resolution. `Wrap<Wrap<u32>>: Show` requires
// matching `impl<T: Show> Show for Wrap<T>` twice and ultimately
// `impl Show for u32`. Codegen produces three distinct mono'd `show`
// functions.
#[test]
fn trait_recursive_wrap_returns_42() {
    let bytes = compile_example("trait_recursive_wrap", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// T2: symbolic dispatch in a generic body via the type-param's bound.
// `t.show()` inside `fn use_show<T: Show>(t: T)` resolves through `T:
// Show` and re-dispatches to the concrete impl at mono time.
#[test]
fn trait_bound_dispatch_returns_42() {
    let bytes = compile_example("trait_bound_dispatch", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Trait surface: declarations, `impl Trait for Type`, blanket `impl<T>
// Trait for &T`, trait bounds on generics. T1 only validates structure;
// dispatch lands in T2.
#[test]
fn trait_decl_and_impl_compiles() {
    let bytes = compile_example("trait_decl_and_impl", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}

// Two ref params share `'a`; the result borrows both.
#[test]
fn lifetime_combined_returns_42() {
    let bytes = compile_example("lifetime_combined", "lib.rs");
    let (mut store, instance) = instantiate(&bytes);
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .expect("export `answer` not found / wrong signature");
    let result = answer.call(&mut store, ()).expect("call failed");
    assert_eq!(result, 42);
}
