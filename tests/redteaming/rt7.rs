// Round 7 of red-team findings — architectural problems surfaced
// after the dyn-trait work (Phases 2-9). Each test documents one
// bug; **the test is expected to fail today** and the failure *is*
// the surfaced bug.
//
// Four underlying defects produce all six problems:
//
//   A. Coercion is enumerated, not principled. `coerce_at` is a
//      parallel entry point to `Subst::coerce` with hand-coded
//      shape branches; both sites-that-need-it AND shapes-that-
//      compose require explicit extension.
//   B. Typeck-to-borrowck communication is a growing pile of side
//      tables. No shared post-typeck IR — each new typeck artifact
//      needs parallel wiring through borrowck.
//   C. Vtable layout is duplicated, not canonical. `dyn_vtable_
//      methods` is queried independently by dispatch and codegen,
//      with dedup happening (or not) at each layer.
//   D. Trait-path canonicalization is per-use-site. (Not directly
//      in rt7, but documented for completeness.)
//
// The phased cleanup plan addresses these defects rather than
// patching symptoms — see `project_dyn_arch_cleanup_plan` in memory.
// Each problem's "fix shape" below points at the matching plan
// phase (X1 / X2 / X3); symptom-only patches are explicitly noted
// as hacks-on-hacks where they'd apply.

use super::*;

// PROBLEM 1: dyn coercion at struct-literal field initializers.
// `coerce_at` (the dyn-aware coercion helper) only runs at four
// hand-listed sites: let-stmt RHS, fn-call args, fn return-tail, fn
// return-expr. `check_struct_lit` coerces each field initializer via
// plain `ctx.subst.coerce` — which doesn't see the dyn pattern.
//
// Defect: A (coercion enumerated, not principled).
//
// Symptom-only patch (HACK): route field initializers through
// `coerce_at` with the field-init node id. Extends the four-site
// enumeration to five. Maintains the parallel coerce_at/coerce
// entry-point split, which is the real defect.
//
// Proper fix: plan Phase X1. Replace `coerce_at` with a recursive
// `try_coerce(expr_id, actual, expected) → Adjustment`; every
// value-flow site (including struct fields) routes through it.
#[test]
fn problem_1_dyn_coercion_at_struct_field() {
    let bytes = try_compile_example(
        "redteaming/rt7/coerce_at_struct_field",
        "lib.rs",
    )
    .expect("expected struct field `&dyn Show` initializer to unsize-coerce `&Foo`");
    let _ = bytes;
}

// PROBLEM 2: dyn coercion at method-call arguments.
// `check_method_call` (and `check_dyn_method_call`) type-check method
// args via plain `ctx.subst.coerce`. Fn-call args were updated in
// Phase 2 to use `coerce_at`; method-call args were missed.
//
// Defect: A (coercion enumerated, not principled).
//
// Symptom-only patch (HACK): swap method-call arg coercion to
// `coerce_at`. Patches drift between fn-call and method-call paths
// without fixing why two paths exist.
//
// Proper fix: plan Phase X1. The recursive `try_coerce` is the only
// entry point; method-call args use it, fn-call args use it,
// drift is structurally impossible.
#[test]
fn problem_2_dyn_coercion_at_method_arg() {
    let bytes = try_compile_example(
        "redteaming/rt7/coerce_at_method_arg",
        "lib.rs",
    )
    .expect("expected method arg `&dyn Show` to unsize-coerce `&Foo`");
    let _ = bytes;
}

// PROBLEM 3: dyn coercion doesn't recurse into compound containers.
// `let t: (&dyn Show, u32) = (&f, 0);` fails because `coerce_at`'s
// shape match requires the OUTER source to be `Ref<T>` and outer
// target to be `Ref<Dyn>`. A tuple wrapper short-circuits that
// match: outer types are `Tuple([Ref<Foo>, u32])` vs `Tuple([Ref<
// Dyn>, u32])` → falls through to plain `unify`, which fails
// per-element.
//
// Defect: A (coercion enumerated, not principled). Specifically:
// `coerce_at` matches only the topmost shape; structural recursion
// through compound containers isn't modelled.
//
// Symptom-only patch (HACK): add a tuple-fold branch to coerce_at
// that walks elements pairwise. Then add a struct-fold branch. Then
// enum-payload. Each container kind is another arm.
//
// Proper fix: plan Phase X1. `try_coerce` includes a
// `StructuralFold` rule that descends through any compound shape
// (tuple, struct, fn-ptr param-list, etc.) pairwise and composes
// child `Adjustment`s. Container kinds aren't enumerated — the
// recursion is the principle.
#[test]
fn problem_3_dyn_coercion_at_tuple_elem() {
    let bytes = try_compile_example(
        "redteaming/rt7/coerce_at_tuple_elem",
        "lib.rs",
    )
    .expect("expected tuple-element `&dyn Show` slot to unsize-coerce `&Foo`");
    let _ = bytes;
}

// PROBLEM 4: chained coercion `&mut dyn → &dyn`.
// `coerce_at` handles single-step unsizings: `&mut T → &mut dyn` (or
// → `&dyn`, downgrading mutability in one shot). But it does NOT
// recognize `&mut dyn Trait → &dyn Trait` once the source is already
// Dyn: the "source already Dyn" guard added in Phase 9 falls
// through to `unify`, which rejects `&mut Dyn` against `&Dyn` as a
// plain mutability mismatch.
//
// Defect: A (coercion enumerated, not principled). Specifically:
// pocket-rust has no first-class reborrow concept distinct from
// coercion — `&mut T → &T` works ad-hoc, not as a uniform rule.
//
// Symptom-only patch (HACK-IN-HACK): add a `&mut Dyn → &Dyn`
// special-case inside the "source already Dyn" special-case branch.
// Each composition (e.g. later `&mut [T] → &[T]`) requires another.
//
// Proper fix: plan Phase X1. The `Adjustment` lattice has a
// `DowngradeMut` step that composes with any other step (Identity,
// UnsizeRef, StructuralFold, ...). `&mut dyn T → &dyn T` becomes
// `DowngradeMut(Identity)`, `&mut Foo → &dyn T` becomes
// `DowngradeMut(UnsizeRef(...))`, and so on — no special-case grid.
#[test]
fn problem_4_mut_dyn_to_dyn_downgrade() {
    let bytes = try_compile_example(
        "redteaming/rt7/double_unsizing_ref_mut_chain",
        "lib.rs",
    )
    .expect("expected `&mut dyn Counter` to downgrade-reborrow as `&dyn Counter`");
    let _ = bytes;
}

// PROBLEM 5: borrowck doesn't trace borrows through dyn coercions.
// Typeck records `DynCoercion` per expr id; borrowck reads the
// `expr_types[id]` (`&dyn Trait`) and emits region constraints
// against the outer dyn-ref's lifetime, NOT against the inner
// concrete ref's source local. The `RefDynCoerce` mono node carries
// `src_concrete_ty` for codegen's vtable, but borrowck never
// consults `dyn_coercions`.
//
// Defect: B (typeck→borrowck via growing side tables). dyn_coercions
// joined the pile of per-NodeId artifacts; nothing forced borrowck
// to consume it.
//
// Symptom-only patch (HACK): wire `dyn_coercions` into borrowck and
// emit region constraints. Closes this leak but normalizes the
// "every new typeck artifact needs ad-hoc borrowck plumbing"
// pattern. The next new artifact will have the same problem.
//
// Proper fix: plan Phase X2 (incremental) feeding into Phase X4
// (long-term). X2: replace `dyn_coercions` with `adjustments` (Phase
// X1's artifact) and have borrowck read region edges off of those.
// X4: a shared post-typeck IR where coercions and resolutions are
// explicit nodes, not side tables — borrowck rebuilds the CFG from
// the IR rather than from AST + N tables.
#[test]
fn problem_5_borrowck_skips_dyn_coercion() {
    let err = try_compile_example(
        "redteaming/rt7/borrowck_skips_dyn_coercion",
        "lib.rs",
    )
    .err()
    .expect("expected borrowck rejection of `&'static dyn Show` derived from a stack local");
    assert!(
        err.contains("lifetime")
            || err.contains("outlive")
            || err.contains("borrow")
            || err.contains("does not live long enough"),
        "expected lifetime / borrow-escape diagnostic, got: {}",
        err,
    );
}

// PROBLEM 6: multi-bound dyn with shared supertrait → spurious
// ambiguity.
// `dyn Show + Tag` where `trait Show: Tag {}`. The vtable walker
// `dyn_vtable_methods` walks each principal's transitive supertrait
// closure independently. For the principal `Show`, the closure
// includes `Tag::tag` (because Show: Tag); for the principal `Tag`,
// the closure includes its own `tag`. Method dispatch sees `tag`
// declared twice — once via Show's closure, once via Tag itself —
// and emits "ambiguous method `tag`".
//
// Defect: C (vtable layout duplicated, not canonical). Dispatch and
// codegen each independently invoke `dyn_vtable_methods`; dedup
// happens incidentally at codegen (slot collapse) but not at
// dispatch (candidate count).
//
// Symptom-only patch (HACK): dedupe `found` in `check_dyn_method_
// call` by `(declaring_trait, method_idx)`. Silences this error
// but leaves the duplicate-slot waste in the vtable, plus the two
// layers still walk independently and could drift on future
// changes (slot ordering, additional metadata).
//
// Proper fix: plan Phase X3. A single canonical `vtable_layout(
// bounds, traits) → VtableLayout` pass produces the deduped,
// ordered slot list. Both dispatch (queries the layout for index)
// and codegen (queries the same layout for packing) consume one
// source of truth. Dedup happens once, at layout time.
#[test]
fn problem_6_multi_bound_shared_supertrait_spurious_ambig() {
    let bytes = try_compile_example(
        "redteaming/rt7/multi_bound_supertrait_spurious_ambig",
        "lib.rs",
    )
    .expect(
        "expected `dyn Show + Tag` (with Show: Tag) to dispatch `tag` unambiguously to the shared declaration",
    );
    let _ = bytes;
}
