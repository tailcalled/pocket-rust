// Callee declares `where 'a: 'b` — the caller may pass `&'a` as
// `&'b` only when the caller's lifetimes can satisfy that. Real
// Rust validates the predicate at every call site: the inferred
// substitution for `'a`/`'b` must satisfy the outlives relation,
// or the call is rejected.
//
// Here, the caller deliberately passes a short-lived ref where
// `'a` would be inferred and a longer-lived ref where `'b` would
// be. The function "borrows" the short ref out as if it had the
// longer lifetime — that's exactly what `'a: 'b` would forbid
// (we'd need 'a outlives 'b, but the caller's 'a is shorter).
// Real Rust rejects the call; the dangling-ish read after the
// inner block ends would otherwise be unsound.
//
// Pocket-rust silently accepts: nothing reads the
// `lifetime_predicates` to check call-site obligations. The
// predicate is parsed and stored but never consumed.
//
// Expected post-fix: at each call site, after the lifetime
// substitution is inferred, the callee's `lifetime_predicates`
// are validated against the substitution.

fn id_through<'a, 'b>(x: &'a u32, _y: &'b u32) -> &'b u32
where
    'a: 'b,
{
    // With 'a: 'b, &'a may shorten to &'b — body is sound when
    // callers actually satisfy the obligation.
    x
}

pub fn answer() -> u32 {
    let long: u32 = 21u32;
    // `inner` lives in a tighter scope; the caller's `'a` (the
    // first arg's lifetime) is SHORTER than `'b` (the second).
    // Real Rust rejects because the predicate `'a: 'b` doesn't
    // hold for this substitution.
    let r: &u32;
    {
        let inner: u32 = 99u32;
        r = id_through(&inner, &long);
    }
    *r + 21u32
}
