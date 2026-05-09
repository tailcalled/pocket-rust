// Function-pointer types `fn(T) -> R`. A bare fn-item name coerces
// into a fn-pointer-typed slot; calling through the value lowers to
// `call_indirect`. Generic fn items can't be addressed without
// specifying type args (gap-tested separately).

use super::*;

// Basic let-annotation coercion: `let f: fn(u32) -> u32 = id;`. The
// bare name `id` resolves through the FuncTable (no local of that
// name shadows it), records a `FnItemAddr` on the expression's id,
// and lowers to `i32.const <table_slot>`. Calling `f(5)` then
// `call_indirect`s the same slot.
#[test]
fn let_anno_fn_ptr_call_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn double(x: u32) -> u32 { x + x }\nfn answer() -> u32 { let f: fn(u32) -> u32 = double; f(21) }",
        )],
        42u32,
    );
}

// Two distinct fn items coerced through the same slot type — the
// codegen path should pick whichever one was assigned to `f`.
#[test]
fn fn_ptr_swap_returns_5() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn one() -> u32 { 1 }\nfn five() -> u32 { 5 }\nfn answer() -> u32 { let f: fn() -> u32 = one; let g: fn() -> u32 = five; g() }",
        )],
        5u32,
    );
}

// FnPtr passed as an argument to another function. The inner call
// site dispatches through the runtime slot, not the static fn name.
#[test]
fn fn_ptr_passed_as_arg_returns_84() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn double(x: u32) -> u32 { x + x }\nfn apply(f: fn(u32) -> u32, n: u32) -> u32 { f(n) }\nfn answer() -> u32 { apply(double, 42) }",
        )],
        84u32,
    );
}

// FnPtr stored in a struct field. The field load yields the i32 slot;
// calling through `s.f(...)`-style would need expression-position
// callees (today's parser only accepts paths), so first bind the
// field's value to a local then call.
#[test]
fn fn_ptr_in_struct_field_returns_100() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "struct Holder { f: fn(u32) -> u32 }\nfn add_one(x: u32) -> u32 { x + 1 }\nfn answer() -> u32 { let h: Holder = Holder { f: add_one }; let f = h.f; f(99) }",
        )],
        100u32,
    );
}

// Two fns of the same shape get the same wasm typeidx — the funcref
// table holds two distinct slots but the typeidx is shared.
#[test]
fn two_fn_ptrs_same_signature_returns_3() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn pick_a() -> u32 { 1 }\nfn pick_b() -> u32 { 2 }\nfn answer() -> u32 { let a: fn() -> u32 = pick_a; let b: fn() -> u32 = pick_b; a() + b() }",
        )],
        3u32,
    );
}

// Negative: fn-ptr types must match arity. `let f: fn(u32) -> u32 = id;`
// where `id` has signature `fn(u32, u32) -> u32` mismatches.
#[test]
fn fn_ptr_arity_mismatch_is_rejected() {
    let err = compile_source(
        "fn add(a: u32, b: u32) -> u32 { a + b }\nfn answer() -> u32 { let f: fn(u32) -> u32 = add; f(1) }",
    );
    assert!(
        err.contains("arity") || err.contains("mismatch"),
        "expected arity-mismatch error, got: {}",
        err
    );
}

// Negative: fn-ptr types must match return type.
#[test]
fn fn_ptr_return_type_mismatch_is_rejected() {
    let err = compile_source(
        "fn flag() -> bool { true }\nfn answer() -> u32 { let f: fn() -> u32 = flag; f() }",
    );
    assert!(
        err.contains("mismatch") || err.contains("expected"),
        "expected return-type mismatch error, got: {}",
        err
    );
}

// Negative: bare-name coercion only fires for fn items. A local of a
// non-FnPtr type can't be coerced to FnPtr.
#[test]
fn local_to_fn_ptr_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: u32 = 5; let f: fn() -> u32 = x; f() }",
    );
    assert!(
        err.contains("mismatch") || err.contains("expected"),
        "expected local-coercion error, got: {}",
        err
    );
}

// Negative: a generic fn item can't be addressed as a fn pointer
// without specifying type args (would need higher-order type-arg
// threading; deferred).
#[test]
fn generic_fn_address_is_rejected() {
    let err = compile_source(
        "fn id<T>(x: T) -> T { x }\nfn answer() -> u32 { let f: fn(u32) -> u32 = id; f(5) }",
    );
    assert!(
        err.contains("generic") || err.contains("type arguments"),
        "expected generic-fn-address error, got: {}",
        err
    );
}
