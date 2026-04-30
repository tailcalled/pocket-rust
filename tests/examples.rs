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

// Compile `examples/<dir>/lib.rs`, instantiate, invoke `<export>`, and
// assert the result. Most tests want `<export>` = `"answer"` and `R` =
// `i32`; the variants (i64 returns, multi-value returns, named exports)
// flow through the same helper.
fn expect_export<R>(dir: &str, export: &str, expected: R)
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

fn expect_answer<R>(dir: &str, expected: R)
where
    R: wasmi::WasmResults + PartialEq + std::fmt::Debug,
{
    expect_export(dir, "answer", expected);
}

#[test]
fn empty_lib_compiles_to_loadable_wasm() {
    let bytes = compile_example("empty", "lib.rs");
    let engine = Engine::default();
    Module::new(&engine, &bytes[..]).expect("wasmi rejected the module");
}

#[test]
fn answer_returns_42() {
    expect_answer("answer", 42i32);
}

#[test]
fn cross_module_call_returns_42() {
    expect_answer("cross_module", 42i32);
}

#[test]
fn nested_calls_returns_300() {
    expect_answer("nested_calls", 300i32);
}

#[test]
fn structs_returns_40() {
    expect_answer("structs", 40i32);
}

#[test]
fn borrows_returns_40() {
    expect_answer("borrows", 40i32);
}

#[test]
fn uses_std_dummy_id_returns_7() {
    expect_answer("uses_std", 7i32);
}

#[test]
fn lets_returns_5() {
    expect_answer("lets", 5i32);
}

#[test]
fn block_expr_returns_11() {
    expect_answer("block_expr", 11i32);
}

#[test]
fn escaping_borrow_returns_42() {
    expect_answer("escaping_borrow", 42i32);
}

#[test]
fn u8_literal_returns_200() {
    expect_answer("u8_literal", 200i32);
}

#[test]
fn i64_literal_returns_9_000_000_000() {
    expect_answer("i64_literal", 9_000_000_000i64);
}

// 128-bit literal goes through `<u128 as Num>::from_i64` which casts
// the i64 argument to u128 — exercising the Wide64 → Wide128 path
// (zero-extending the high half for unsigned target).
#[test]
fn u128_literal_returns_42() {
    expect_answer("u128_literal", (42i64, 0i64));
}

// When `impl Show for u32` and `impl<T> Show for &T` both provide
// `show`, dispatch on `r: &u32; r.show()` should pick the `&T` impl —
// it matches the receiver directly (peel_level 0), while the `u32`
// impl only matches after peeling (peel_level 1). The `&T` blanket
// returns 2.
#[test]
fn autoref_disambig_through_ref_returns_2() {
    expect_export("autoref_disambig", "through_ref", 2i32);
}

// Sanity check the inverse: with `x: u32` (owned), `impl Show for u32`
// matches directly at tier 0 while `impl<T> Show for &T` matches via
// pattern-side autoref at tier 1. The direct match wins.
#[test]
fn autoref_disambig_through_owned_returns_1() {
    expect_export("autoref_disambig", "through_owned", 1i32);
}

// Pattern-side autoref reaching a blanket impl: only `impl<T> Tag for
// &T` exists, recv is owned `x: u32`. Pattern `&T` matches via autoref
// (tier 1, T=u32). derive_recv_adjust says BorrowImm — `&x` is passed
// to the impl method.
#[test]
fn autoref_only_returns_7() {
    expect_answer("autoref_only", 7i32);
}

// Sign-extension test: cast u64 (with bit 63 set) → i64 (reinterprets
// as i64::MIN) → i128. The 128-bit high half should be all-ones, since
// the source is signed and negative.
#[test]
fn i128_sign_extend_returns_i64_min() {
    expect_answer("i128_sign_extend", (i64::MIN, -1i64));
}

#[test]
fn let_mut_scalar_returns_99() {
    expect_answer("let_mut_scalar", 99i32);
}

#[test]
fn let_mut_record_returns_99() {
    expect_answer("let_mut_record", 99i32);
}

#[test]
fn let_mut_nested_returns_99() {
    expect_answer("let_mut_nested", 99i32);
}

#[test]
fn int_inference_returns_7() {
    expect_answer("int_inference", 7i32);
}

#[test]
fn mut_ref_through_binding_returns_99() {
    expect_answer("mut_ref_through_binding", 99i32);
}

#[test]
fn mut_ref_direct_returns_50() {
    expect_answer("mut_ref_direct", 50i32);
}

#[test]
fn mut_ref_field_returns_77() {
    expect_answer("mut_ref_field", 77i32);
}

#[test]
fn inner_borrow_lifetime_returns_5() {
    expect_answer("inner_borrow_lifetime", 5i32);
}

#[test]
fn borrow_field_returns_42() {
    expect_answer("borrow_field", 42i32);
}

#[test]
fn methods_returns_42() {
    expect_answer("methods", 42i32);
}

#[test]
fn generic_id_returns_100() {
    expect_answer("generic_id", 100i32);
}

#[test]
fn generic_pair_returns_7() {
    expect_answer("generic_pair", 7i32);
}

#[test]
fn copy_double_use_returns_7() {
    expect_answer("copy_double_use", 7i32);
}

#[test]
fn place_borrow_noncopy_field_returns_7() {
    expect_answer("place_borrow_noncopy_field", 7i32);
}

#[test]
fn place_borrow_through_ref_returns_42() {
    expect_answer("place_borrow_through_ref", 42i32);
}

#[test]
fn nll_sequential_borrows_returns_7() {
    expect_answer("nll_sequential_borrows", 7i32);
}

#[test]
fn nll_borrow_then_move_returns_7() {
    expect_answer("nll_borrow_then_move", 7i32);
}

#[test]
fn uses_std_generic_struct_returns_42() {
    expect_answer("uses_std_generic_struct", 42i32);
}

#[test]
fn uses_std_generic_returns_42() {
    expect_answer("uses_std_generic", 42i32);
}

// Named lifetimes on functions tie param to return type. `pick_first<'a>`
// picks `x`'s lifetime; the elided y arg gets a fresh inferred one and
// doesn't constrain the result.
#[test]
fn lifetime_named_returns_42() {
    expect_answer("lifetime_named", 42i32);
}

// Refs in struct fields: a generic `Wrapper<'a>` holds `&'a Inner` and a
// field-access produces the held borrow.
#[test]
fn lifetime_struct_field_returns_42() {
    expect_answer("lifetime_struct_field", 42i32);
}

// `&'a self` receiver tied to the impl's lifetime param routes the
// receiver's borrow into the return ref.
#[test]
fn lifetime_self_receiver_returns_42() {
    expect_answer("lifetime_self_receiver", 42i32);
}

// Partial-concrete impl: `impl<T> Pair<usize, T>` matches `Pair<u32, T>`
// for any T's substitution. Method dispatches via try_match on impl_target.
#[test]
fn impl_partial_concrete_returns_42() {
    expect_answer("impl_partial_concrete", 42i32);
}

// Repeat-param impl: `impl<T> Pair<T, T>` only matches when both type
// args coincide. Matching binds T once and unifies the second occurrence.
#[test]
fn impl_repeat_param_returns_42() {
    expect_answer("impl_repeat_param", 42i32);
}

// Fully-concrete impl: zero impl type-params, target is concrete.
#[test]
fn impl_fully_concrete_returns_42() {
    expect_answer("impl_fully_concrete", 42i32);
}

// T4.5: drops at block-expression scope end. The inner block has a
// tail (`42`) and a Drop binding (`_l`). Codegen saves the tail value
// to a local, runs `_l`'s drop (writes 7 to c), reloads the tail, then
// the outer fn reads `c`.
#[test]
fn drop_block_expr_returns_7() {
    expect_answer("drop_block_expr", 7i32);
}

// T4.5: Drop function parameters. `take(l: Logger)` drops `l` at fn
// end (after returning 42). The outer reads `c` after `take` returns
// and observes the drop side effect.
#[test]
fn drop_fn_param_returns_1() {
    expect_answer("drop_fn_param", 1i32);
}

// T2.5b: trait methods with their own type-params. `Pick::pick<U>`
// declares a method-level type-param. The impl on `First` carries a
// matching `<U>` (validated α-equivalently). At a symbolic call
// `t.pick::<u32>(11, 22)` through a `T: Pick` bound, codegen monomorphizes
// `First::pick<u32>` — the literal args land on `u32` (via Num), and
// the receiver `First` lands on the concrete impl after `solve_impl`.
#[test]
fn trait_method_generic_returns_11() {
    expect_answer("trait_method_generic", 11i32);
}

// Lifetime cleanup: nested per-slot field borrow tracking. Reading
// `o.i.r` follows a multi-segment field path through a struct
// containing a struct-with-ref. Borrowck records the inner borrow at
// the outer holder under the nested path `["i", "r"]`, and reads
// through that path return the borrow correctly. The runtime side
// just verifies the value flows through.
#[test]
fn nested_field_borrow_returns_42() {
    expect_answer("nested_field_borrow", 42i32);
}

// Lifetime cleanup: anonymous `'_` lifetime. `'_` parses to a fresh
// `Inferred(0)` placeholder per occurrence (rather than a regular
// `Named("_")`), so it works in let-binding annotations and impl
// targets without users having to invent a unique `'a` name. Here
// `impl Drop for Logger<'_>` and `let _l: Logger<'_> = ...` both rely
// on it.
#[test]
fn anon_lifetime_impl_returns_42() {
    expect_answer("anon_lifetime_impl", 42i32);
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
    expect_answer("drop_moved", 0i32);
}

// T2.6.5: when type-pattern matching yields multiple candidates, drop
// those whose `derive_recv_adjust` would error (e.g. method takes
// `self` by value but recv is a borrow). Here both `impl Show for Foo`
// and `impl<T: Show> Show for &T` type-match `&Foo`, but only the
// blanket can adjust — the inherent would move out of borrow.
#[test]
fn dispatch_adjust_filter_returns_99() {
    expect_answer("dispatch_adjust_filter", 99i32);
}

// T2.6: concrete trait dispatch on a primitive recv. `x.show()` for
// `x: u32` finds `impl Show for u32` even though the impl_target isn't
// a struct path (so the old `find_method_candidates` filter would have
// missed it).
#[test]
fn trait_impl_on_u32_returns_42() {
    expect_answer("trait_impl_on_u32", 42i32);
}

// T2.6: blanket impl `impl<T> Show for &T` dispatches when the recv is
// a `&Foo` and Foo doesn't otherwise implement Show. Verifies the
// non-struct target path through method-call dispatch.
#[test]
fn trait_blanket_on_ref_returns_42() {
    expect_answer("trait_blanket_on_ref", 42i32);
}

// T5.5: integer literal lands on a user type via `impl Num for Wrap`.
// `let w: Wrap = 42` resolves the literal to `Wrap` (instead of erroring
// because Wrap isn't an integer kind), and codegen routes through
// `<Wrap as Num>::from_i64`.
#[test]
fn num_user_type_returns_42() {
    expect_answer("num_user_type", 42i32);
}

// T5.5: integer literal in a `<T: Num>` generic body. Inside `make<T:
// Num>() -> T { 42 }`, the literal lands on `T` (Param-typed); at mono
// time `T` resolves to `u32`, and the literal codegens as
// `<u32 as Num>::from_i64(42)`.
#[test]
fn num_generic_body_returns_42() {
    expect_answer("num_generic_body", 42i32);
}

// T5: every integer literal desugars to `<T as Num>::from_i64(value)`.
// This test exercises u8, i64, and u32 literal codegen end-to-end —
// each literal becomes a real call to the relevant `from_i64` impl
// (no inlining), and the values flow through to the answer.
#[test]
fn num_literal_dispatch_returns_42() {
    expect_answer("num_literal_dispatch", 42i32);
}

// T4: dropping a Logger at the inner block's scope end writes 1 to a
// shared counter via `*self.counter = 1`. The outer fn reads `c` after
// the block, observing the drop side effect.
#[test]
fn drop_logger_returns_1() {
    expect_answer("drop_logger", 1i32);
}

// T2.5: trait dispatch through `&self` autoref'ing an owned generic
// receiver. `t.get()` inside `fn use_get<T: Get>(t: T)` where Get takes
// `&self` must autoref `t` before the trait call.
#[test]
fn trait_borrow_self_dispatch_returns_42() {
    expect_answer("trait_borrow_self_dispatch", 42i32);
}

// T2.5: `impl<T: Copy> Copy for Wrap<T> {}` validates: the bound makes
// `Param(T)` Copy so the `inner: T` field passes the field-Copy check.
#[test]
fn copy_generic_with_bound_returns_42() {
    expect_answer("copy_generic_with_bound", 42i32);
}

// T2.5: in a generic body with `T: Copy`, reading `t` after `let s = t`
// is a value copy (not a move) because the bound makes `Param(T)` Copy.
#[test]
fn copy_param_via_bound_returns_42() {
    expect_answer("copy_param_via_bound", 42i32);
}

// T3: user-defined `impl Copy for Pt {}`. Reading `p` after `let q = p`
// should be allowed since Pt is Copy.
#[test]
fn copy_user_struct_returns_42() {
    expect_answer("copy_user_struct", 42i32);
}

// T3: `&mut T` is NOT Copy — assigning a mut-ref to another binding
// moves it (preserves exclusivity). Reading the original after move
// would be rejected; this test passes the move via the new binding.
#[test]
fn copy_mut_ref_not_copy_returns_42() {
    expect_answer("copy_mut_ref_not_copy", 42i32);
}

// T2: concrete trait method dispatch via `impl Show for Foo` + `f.show()`.
#[test]
fn trait_concrete_dispatch_returns_42() {
    expect_answer("trait_concrete_dispatch", 42i32);
}

// T2: recursive impl resolution. `Wrap<Wrap<u32>>: Show` requires
// matching `impl<T: Show> Show for Wrap<T>` twice and ultimately
// `impl Show for u32`. Codegen produces three distinct mono'd `show`
// functions.
#[test]
fn trait_recursive_wrap_returns_42() {
    expect_answer("trait_recursive_wrap", 42i32);
}

// T2: symbolic dispatch in a generic body via the type-param's bound.
// `t.show()` inside `fn use_show<T: Show>(t: T)` resolves through `T:
// Show` and re-dispatches to the concrete impl at mono time.
#[test]
fn trait_bound_dispatch_returns_42() {
    expect_answer("trait_bound_dispatch", 42i32);
}

// Trait surface: declarations, `impl Trait for Type`, blanket `impl<T>
// Trait for &T`, trait bounds on generics. T1 only validates structure;
// dispatch lands in T2.
#[test]
fn trait_decl_and_impl_compiles() {
    expect_answer("trait_decl_and_impl", 42i32);
}

// Two ref params share `'a`; the result borrows both.
#[test]
fn lifetime_combined_returns_42() {
    expect_answer("lifetime_combined", 42i32);
}

// Basic `use`: bring `std::dummy::id` into scope as `id`. The function
// call `id(7)` uses the use-table to resolve to `["std","dummy","id"]`
// rather than the current-module-relative `["id"]`.
#[test]
fn use_basic_returns_7() {
    expect_answer("use_basic", 7i32);
}

// Glob: `use std::dummy::*;` brings every item directly under
// `std::dummy` into scope. `id(42)` resolves via the glob.
#[test]
fn use_glob_returns_42() {
    expect_answer("use_glob", 42i32);
}

// Rename: `use std::dummy::id as identity;`. The local name
// `identity` resolves to the imported full path.
#[test]
fn use_rename_returns_99() {
    expect_answer("use_rename", 99i32);
}

// Brace multi-import: `use std::{Drop, dummy};` brings both `Drop`
// (trait, used in an impl block) and `dummy` (module, used as a
// path prefix `dummy::id`) into scope.
#[test]
fn use_brace_returns_42() {
    expect_answer("use_brace", 42i32);
}

// Block-scope use: `use std::dummy::id;` inside a block expression
// scopes the import to that block. `id(33)` resolves via the local
// scope. (Adding the same import outside the block would also work,
// but this verifies the block-scope plumbing.)
#[test]
fn use_block_scope_returns_33() {
    expect_answer("use_block_scope", 33i32);
}

// `use crate::…` resolves through the enclosing crate's root. For
// the user crate (name == ""), `crate::helper::compute` rewrites to
// `helper::compute`. This verifies the crate-prefix substitution in
// `flatten_use_tree`.
#[test]
fn use_crate_returns_77() {
    expect_answer("use_crate", 77i32);
}

// `pub use` re-exports. Module `b` has `pub use crate::a::deep;`,
// which makes `b::deep` resolve (from outside `b`) to `a::deep`.
// The caller writes `b::deep()` and it dispatches to the original
// definition. Verifies the ReExportTable lookup at call sites.
#[test]
fn pub_use_reexport_returns_77() {
    expect_answer("pub_use_reexport", 77i32);
}

// Booleans + if-expression: `if b { 1 } else { 2 }` with `b: bool`.
// Verifies bool literal codegen, the wasm if/else block, and bool
// flow through a function parameter.
#[test]
fn bool_if_returns_1() {
    expect_answer("bool_if", 1i32);
}

// Multi-value `if` result: a u128 flattens to two i64s, so the
// wasm if/else block must reference a registered FuncType (no
// params, two i64 results) by typeidx. Codegen registers it on
// the fly via `pending_types`, drained into wasm_mod.types at
// function-emit-end. Returns 42u128 = (low=42, high=0).
#[test]
fn if_returns_u128() {
    expect_answer("if_returns_u128", (42i64, 0i64));
}

// Multi-value `if` returning a struct that flattens to (i32, i64).
// Same typeidx-registration path as u128, with mixed valtypes.
// Picks the then-arm (`b=true`); reads `p.b` = 9000000000.
#[test]
fn if_returns_struct() {
    expect_answer("if_returns_struct", 9_000_000_000i64);
}

// Conditional Drop: `l: Logger` is moved into `consume(l)` in the
// then-arm but not the else-arm. Borrowck records its post-merge
// status as MaybeMoved; codegen allocates a runtime drop flag,
// initialized to 1 at l's let-stmt, cleared to 0 at the move site.
// The fn-end drop checks the flag — when b=true (the move arm)
// the outer drop is skipped, so total drops = 1 (the one inside
// `consume`). After 1 drop: sink := original counter = 5, counter
// := 1. Without flags this would be 2 drops; second drop sets
// sink := 1 (current counter), so the answer would be 1.
#[test]
fn if_conditional_drop_returns_5() {
    expect_answer("if_conditional_drop", 5u32);
}

// Drop binding moved in BOTH arms — borrowck's intersection rule
// gives final status `Moved` (not MaybeMoved). Codegen skips the
// outer drop entirely (no flag needed). Drops total = 1 (inside
// consume). sink ends up = 5 (original counter).
#[test]
fn if_drop_moved_in_both_returns_5() {
    expect_answer("if_drop_moved_in_both", 5u32);
}

// Drop binding moved in NEITHER arm — status is `Init` post-merge.
// Codegen drops unconditionally at the binding's scope-end (no
// flag). Drops total = 1 (the outer one). sink ends up = 5.
#[test]
fn if_drop_moved_in_neither_returns_5() {
    expect_answer("if_drop_moved_in_neither", 5u32);
}

// Borrows flow through if-tail. Both arms produce `&'a u32`; the
// if-expression's value carries the union of arm borrows so the
// caller's let-binding correctly tracks borrows on both possible
// sources.
#[test]
fn if_returns_borrow_returns_42() {
    expect_answer("if_returns_borrow", 42i32);
}

// Generic `T` flowing through an if. `pick<T>(b, x, y) -> T` walks
// polymorphically; codegen monomorphizes per call site. The if's
// result type is `Param("T")`, which substitutes to `u32` here —
// single-scalar at mono time, so the BlockType is `Single(I32)`.
#[test]
fn if_generic_t_returns_42() {
    expect_answer("if_generic_t", 42i32);
}

// Same generic if-pick, but monomorphized to `u128` — which
// flattens to (i64, i64). Verifies the multi-value typeidx path
// fires correctly when generic substitution lands on a wide type.
#[test]
fn if_generic_t_u128_returns_42() {
    expect_answer("if_generic_t_u128", (42i64, 0i64));
}
