// Closures inside generic functions can't reference the enclosing
// fn's type-params. `check_closure` resolves the closure params'
// type annotations against `ctx.type_params` (which IS the enclosing
// fn's type-param list), so during the *initial* body typeck `T`
// resolves correctly. But the synthesized impl method registered by
// `register_synthesized_closure_impl` has no type-params — it's a
// concrete method on the concrete `__closure_<id>` struct. When
// `check_function` re-types the synthesized method's body (which
// contains the closure body cloned in), `T` isn't in scope and the
// resolution fails with "unknown type: T".
//
// Architectural shape: `closure_lower` doesn't propagate the
// enclosing template's type-params to the synthesized struct + impl.
// For a closure inside `fn helper<T>(x: T)`, the synthesized struct
// should be `__closure_<id><T>` (carrying the same type-params), the
// impl should be `impl<T> Fn<(T,)> for __closure_<id><T>`, and the
// method body's `T` references resolve against the impl's type-
// params at the synth method's check_function call.
//
// The fix has two parts: (1) `register_closure_structs` (typeck
// post-pass that adds `StructEntry` rows) needs to copy the
// enclosing fn's type-params onto the synth struct; (2)
// `synthesize_impl_for_closure` in `closure_lower` needs to
// propagate them onto the impl's `type_params` and the impl-method's
// signature. The closure's `ClosureInfo` would gain an
// `enclosing_type_params: Vec<String>` field captured at typeck
// time.
//
// Without this, every generic-bearing closure use case fails —
// `fn map<T, F: Fn(T) -> T>(...)` callers, generic helpers,
// monomorphic-via-generics patterns. The `selfhost` target hits this
// the moment any of pocket-rust's own generic helpers (which exist
// in `src/typeck/`, `src/borrowck/`, etc.) introduce a closure.
//
// Expected post-fix: program compiles and `answer()` returns 42.

fn helper<T>(x: T) -> T {
    let f = |y: T| y;
    f.call((x,))
}

pub fn answer() -> u32 {
    helper(42u32)
}
