// The function's signature *says* `'a: 'b` (so `&'a u32` can stand
// in for `&'b u32`), but the BODY does the opposite — returns
// the `'b` ref where `&'a u32` is required. Real Rust's borrowck
// rejects: the body's `_y` has lifetime `'b`, but it's being
// returned as `&'a u32`, requiring `'b: 'a` — opposite of what
// the where-clause declared.
//
// Pocket-rust silently accepts. The where-clause's resolved
// `LifetimePredResolved` lives on the FnSymbol in `lifetime_predicates`
// but no consumer ever reads it; borrowck's lifetime checking is
// "Phase B structural-only" and doesn't solve outlives obligations.
// The predicate is dead storage.
//
// Expected post-fix: borrowck treats `lifetime_predicates` as
// declared facts about the in-scope lifetimes' relations, and
// rejects any function-body lifetime relation that contradicts the
// predicate (or, more weakly, requires the predicate to make a
// type-check pass).

fn lift<'a, 'b>(x: &'a u32, y: &'b u32) -> &'a u32
where
    'a: 'b,
{
    // Body wants to return &'a; we have x: &'a (good) but we
    // mis-return y: &'b. The `'a: 'b` predicate doesn't help —
    // we'd need `'b: 'a` for this to type-check.
    let _ = x;
    y
}

pub fn answer() -> u32 {
    let a: u32 = 21u32;
    let b: u32 = 21u32;
    *lift(&a, &b) + 21u32
}
