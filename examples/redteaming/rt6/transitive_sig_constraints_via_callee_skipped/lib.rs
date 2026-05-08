// Caller-side constraints that hold transitively through a callee's
// body-fresh region are silently dropped.
//
// L4's solver skips required edges where either endpoint is
// body-fresh: "the solver picks any value for the body-fresh region
// that satisfies." That's correct in isolation but loses transitive
// constraints between two SIG-FIXED regions when the chain runs
// through a body-fresh middle.
//
// Setup: callee `pick<'a>(x: &'a u32, _: &'a u32) -> &'a u32` —
// both args share `'a`, return is `'a`. At a caller's call site,
// `'a_inst` is body-fresh in the caller. Caller's two sig-fixed
// lifetimes `'p` and `'q` flow into both args:
//   * `'p : 'a_inst` (CallArg)
//   * `'q : 'a_inst` (CallArg)
//   * `'a_inst : 'ret_caller` (CallReturn)
//
// Real Rust derives via region elimination: setting `'a_inst` to the
// shorter of `'p`/`'q`, the return must outlive `'ret_caller`, so the
// shorter of `'p`/`'q` must outlive `'ret_caller`. Without `'q : 'p`
// declared (and `'ret_caller = 'p`), real Rust rejects the caller.
//
// L4 today: `'a_inst` is body-fresh; every edge touching it is
// skipped. The caller-side requirement `'q : 'p` (transitive through
// `'a_inst`) is never derived; the body slips.
//
// Architectural shape: body-fresh regions can act as existential
// variables. The solver must ELIMINATE them to derive sig-only edges
// rather than skipping them entirely. For each pair of sig-fixed
// regions (S1, S2), check whether ANY chain of declared+required
// edges (with body-fresh intermediates) implies `S1 : S2` in the
// caller. Floyd-Warshall over the full edge set (treating both
// declared and required edges as facts), then verify required
// sig-fixed-only edges in that closure, would catch this.
//
// Expected post-fix: extend `regions::solve` to compute the closure
// over the full edge set (declared + required), with body-fresh
// regions as transit nodes. The check for each required sig-fixed
// edge becomes "is it in this larger closure?" The trick is keeping
// the body-fresh-as-free-variable semantics for SIG vs body-fresh
// edges (those still skip), while still deriving sig-vs-sig
// transitive requirements.

fn pick<'a>(x: &'a u32, _y: &'a u32) -> &'a u32 {
    x
}

fn caller<'p, 'q>(x: &'p u32, y: &'q u32) -> &'p u32 {
    // Real Rust rejects: `pick`'s `'a` unifies with both `'p` and
    // `'q`, so the return is bounded by the shorter — which can't be
    // proved to outlive `'p` unless the caller declares `where 'q: 'p`.
    pick(x, y)
}

pub fn answer() -> u32 {
    let a: u32 = 21u32;
    let b: u32 = 21u32;
    *caller(&a, &b) + 21u32
}
