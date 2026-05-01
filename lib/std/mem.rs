// Memory primitives. Mirrors the surface of Rust's `std::mem`.
//
// `drop(x)` takes T by value and lets it fall out of scope, running
// T's destructor (if T: Drop). Because `drop` is generic, each
// instantiation is monomorphized — the codegen sees a concrete T at
// emit time, identifies whether `x` is a Drop binding, and either
// emits the synthetic `<T as Drop>::drop(&mut x)` call at function-end
// or a no-op. The implementation is just an empty body.

pub fn drop<T>(_x: T) {}

// `size_of::<T>()` returns the byte size of T as a `usize`. Wraps the
// `¤size_of::<T>()` intrinsic — the resolved T is concrete by mono
// time, so this is a compile-time constant.
pub fn size_of<T>() -> usize {
    ¤size_of::<T>()
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically.
//
// TODO: forget(x) — needs a way to suppress Drop on a binding; today's Drop machinery always fires at function-end on Init bindings.
// TODO: replace(dst, src) — needs returning the old value while overwriting via `&mut T`; expressible once a clear use case appears.
// TODO: size_of_val(&v) — needs a way to inspect the value's runtime size; for Sized types (all of pocket-rust's so far) this is trivially `size_of::<T>()`, so add when there's a caller.
// TODO: swap(a, b) — expressible via temporaries today; not a priority.
// TODO: take(dst) — needs `Default::default()`, which needs a `Default` trait + impls.
// TODO: transmute — too dangerous to expose without a clear bootstrap need.
// TODO: zeroed() / uninitialized() — needs `MaybeUninit` machinery for safety.
