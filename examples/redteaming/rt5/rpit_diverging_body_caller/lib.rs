// rt4#1's fix accepts RPIT bodies that diverge (`!` doesn't have to
// satisfy the bound — `!` is uninhabited and any obligation is
// vacuously true). The pin gets recorded as `RType::Never`, then
// post-typeck `finalize_rpit_substitutions` rewrites the function's
// stored return type and every recorded `MethodResolution.trait_dispatch.recv_type`
// from `Opaque{f, 0}` to `Never`.
//
// But callers of an always-diverging RPIT fn still go through trait
// dispatch. The recorded dispatch's recv_type now resolves to
// `Never` at mono — and `solve_impl(Show, Never)` returns None
// because no impl row's target is `!`. Mono errors:
//   "no impl of std.ops.Show for ! at lowering"
// even though typeck accepted the body.
//
// Real Rust accepts this fine: `!` coerces to anything, and the
// caller's `.show()` site is on a never-actually-reached path of
// the program (the divergence happens before).
//
// Expected post-fix: this compiles. Either solve_impl needs an
// `RType::Never` arm (treat `!` as satisfying every trait), or the
// finalize substitution should NOT rewrite recv_type for diverging
// pins — trait dispatch should keep using the slot's bounds path.

trait Show {
    fn show(self) -> u32;
}

fn make() -> impl Show {
    panic!("never reached")
}

pub fn answer() -> u32 {
    make().show()
}
