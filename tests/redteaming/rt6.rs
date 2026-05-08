// Round 6 of red-team findings — architectural problems surfaced
// after the lifetime-subtyping work (Phases L0–L5). Each test
// documents one bug; **the test is expected to fail today** and the
// failure *is* the surfaced bug.
//
// rt6 covers the gaps that the loose-but-correct region solver and
// the not-yet-variance-aware constraint emitter leave open. Each
// problem's docstring names the architectural shape so a fix targets
// the right layer rather than patching the symptom.

use super::*;

// PROBLEM 1: method-call call sites bypass region inference entirely.
// `src/borrowck/build.rs::emit_call_constraints` short-circuits with
// `CallTarget::MethodResolution(_) => return`. Free-fn calls
// (CallTarget::Path) emit per-arg / where-clause / return edges
// against the callee's signature; method calls emit nothing.
//
// Architectural shape: the call-site region pass treats methods as
// outside its model. Method dispatch resolution is recorded on
// `MethodResolution` (looked up by NodeId), and pulling the callee's
// `lifetime_params` / `lifetime_predicates` / param-and-return types
// through that record requires the same plumbing that the Path arm
// already does. The fix is to widen the MethodResolution arm to do
// the same instantiation + constraint emission.
//
// Without this, a generic caller can call a method whose
// where-clause it can't prove, and slip past the borrow checker.
#[test]
fn problem_1_method_call_outlives_unenforced() {
    let err = try_compile_example(
        "redteaming/rt6/method_call_outlives_unenforced",
        "lib.rs",
    )
    .err()
    .expect("expected method body returning &'a as &'b without `where 'a: 'b` to be rejected");
    assert!(
        err.contains("lifetime")
            || err.contains("outlives")
            || err.contains("does not live long enough"),
        "expected lifetime-mismatch diagnostic, got: {}",
        err,
    );
}

// PROBLEM 2: impl-block-level lifetime where-clauses are silently
// dropped. `src/typeck/setup.rs` processes `ib.where_clause` only for
// `WherePredicate::Type` — `WherePredicate::Lifetime` falls through
// (the comment in setup.rs even labels them "parsed but not yet
// enforced"). When borrowck reads a method's `lifetime_predicates`,
// it sees only the FUNCTION's own where-clause.
//
// Architectural shape: rt4#2 fixed the analogous gap for type
// predicates by merging `ib.where_clause`'s Param-LHS predicates into
// `impl_type_param_bounds`. Lifetime predicates need the parallel
// merge into the per-method `lifetime_predicates` (or onto the impl
// itself with read-through at borrowck setup). The Lifetime-arm
// fall-through is the architectural defect: that arm needs a body.
//
// Real Rust accepts an impl-level `where 'a: 'b` and applies it to
// every method in the impl. Today's pocket-rust forces every method
// to repeat the predicate.
#[test]
fn problem_2_impl_lifetime_where_clause_dropped() {
    let bytes = try_compile_example(
        "redteaming/rt6/impl_lifetime_where_clause_dropped",
        "lib.rs",
    )
    .expect(
        "expected impl-level `where 'a: 'b` to apply to methods in the impl, allowing the body to compile",
    );
    let _ = bytes;
}

// PROBLEM 3: variance vectors are computed but never read at
// value-flow boundaries. L0 populates `type_param_variance` and
// `lifetime_param_variance` on every StructEntry/EnumEntry; L3's
// `place_outer_region` (and by extension `emit_assign_constraints` /
// `emit_call_constraints`) only inspects the OUTERMOST `RType::Ref`.
// Struct-typed bindings — `Holder<'a>`, `Vec<&'a T>`, `Option<&'a T>`
// — return None from `place_outer_region` even when their inner
// lifetimes are sig-fixed.
//
// Architectural shape: variance was added "up-front" (L0) so borrowck
// could read it at the value-flow boundary that L3 introduces. L3's
// constraint emitter doesn't consult those vectors at all — it only
// compares outermost regions. Inner-lifetime constraints, which are
// exactly what variance is FOR, never get derived.
//
// Expected post-fix: an `emit_value_flow_constraints(src_ty, dst_ty,
// span, source)` helper that walks paired positions in source and
// destination types. At each region-bearing slot, look up the slot's
// variance on the declaring struct/enum's vectors; emit a one-way
// outlives edge for Covariant slots, two edges (equate) for
// Invariant. For type-arg slots of generic structs, recurse with
// composed variance. Replace direct `place_outer_region` comparisons
// in `emit_assign_constraints` (Use/Borrow arms) and
// `emit_call_constraints` (per-arg, return) with calls to this
// helper.
//
// Real Rust rejects the example fn: returning `Holder<'a>` as
// `Holder<'b>` requires `'a: 'b` (Holder is covariant in `'a`); not
// declared.
#[test]
fn problem_3_struct_typed_value_flow_skips_variance() {
    let err = try_compile_example(
        "redteaming/rt6/struct_typed_value_flow_skips_variance",
        "lib.rs",
    )
    .err()
    .expect("expected `Holder<'a>` flowing into `Holder<'b>` without `'a: 'b` to be rejected");
    assert!(
        err.contains("lifetime")
            || err.contains("outlives")
            || err.contains("does not live long enough"),
        "expected lifetime-mismatch diagnostic, got: {}",
        err,
    );
}

// PROBLEM 4: caller-side transitive constraints through a callee's
// body-fresh region are silently dropped. L4's solver skips required
// edges where either endpoint is body-fresh ("solver picks any value
// that satisfies"). Correct in isolation; wrong when two sig-fixed
// caller regions are linked transitively via a callee's body-fresh
// region.
//
// Setup: callee `pick<'a>(x: &'a u32, _: &'a u32) -> &'a u32` — both
// args share `'a`. At the caller's call site, `'a_inst` is
// body-fresh. Caller has sig-fixed `'p`, `'q`; emits:
//   * `'p : 'a_inst` (CallArg)       ← skipped (body-fresh endpoint)
//   * `'q : 'a_inst` (CallArg)       ← skipped
//   * `'a_inst : 'p_caller_ret` (CallReturn) ← skipped
// Each edge has a body-fresh endpoint, so each is skipped. The
// transitive sig-only consequence `'q : 'p` (which real Rust derives
// via region-variable elimination and rejects without a `where 'q:
// 'p` declaration) is never derived.
//
// Architectural shape: body-fresh regions are existential variables,
// not "skip flags". The solver should eliminate them to derive
// sig-only required edges. Floyd-Warshall over the FULL edge set
// (declared + required, with body-fresh as transit nodes), then
// requirement check on sig-only pairs, would close this.
//
// Real Rust rejects `caller`. Today's pocket-rust accepts.
#[test]
fn problem_4_transitive_sig_constraints_via_callee_skipped() {
    let err = try_compile_example(
        "redteaming/rt6/transitive_sig_constraints_via_callee_skipped",
        "lib.rs",
    )
    .err()
    .expect("expected caller passing distinct lifetimes to a unified-`'a` callee to be rejected");
    assert!(
        err.contains("lifetime")
            || err.contains("outlives")
            || err.contains("does not live long enough"),
        "expected lifetime-mismatch diagnostic, got: {}",
        err,
    );
}
