// Variance vectors are computed but never read at value-flow sites.
//
// L0 added `type_param_variance` and `lifetime_param_variance` to
// every StructEntry/EnumEntry, populated by `compute_variance`. L3's
// constraint emitter is supposed to consult these at every value-flow
// boundary between same-path types with differing region/type args
// (Covariant slot → one-way edge; Invariant → equate). It doesn't.
//
// `place_outer_region` (in `src/borrowck/build.rs`) only inspects the
// outermost layer: `RType::Ref` returns its lifetime's region;
// anything else returns None. Struct-typed bindings — `Holder<'a>`,
// `Vec<&'a T>`, `Option<&'a T>`, etc. — return None even when their
// inner lifetimes are sig-fixed. So `let h2: Holder<'b> = h1;` (with
// `h1: Holder<'a>`) emits no constraints; the `'a: 'b` requirement
// that variance would derive is silently dropped.
//
// Expected post-fix: extend `place_outer_region` (or pair it with a
// new `emit_value_flow_constraints(src_ty, dst_ty)` helper) to walk
// matching positions in source and destination types. At each
// region-bearing slot, look up the slot's variance on its declaring
// struct/enum and emit the appropriate edge(s). For type-arg slots
// of generic structs, recurse with composed variance.
//
// Real Rust rejects this fn — the body returns `Holder<'a>` as
// `Holder<'b>` without `'a: 'b` declared.

struct Holder<'a> {
    inner: &'a u32,
}

fn relax<'a, 'b>(h: Holder<'a>) -> Holder<'b> {
    h
}

pub fn answer() -> u32 {
    let v: u32 = 21u32;
    let h: Holder<'_> = Holder { inner: &v };
    let h2: Holder<'_> = relax(h);
    *h2.inner + 21u32
}
