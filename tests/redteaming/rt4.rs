// Round 4 of red-team findings — architectural bugs surfaced after
// RPIT and where-clause work. Each test below documents one bug;
// **the test is expected to fail today** and the failure *is* the
// surfaced bug.
//
// rt4 patterns:
//   * "valid program rejected" — a body or signature shape Rust
//     accepts but pocket-rust errors.
//   * "invalid program accepted" — a constraint pocket-rust silently
//     drops, letting through programs Rust would reject at the
//     declaration site.
//
// Each problem's docstring names the architectural shape so a fix can
// target the right layer rather than patching the symptom.

use super::*;

// PROBLEM 1: RPIT body that diverges (returns `!`) is rejected. The
// post-unify validation in `check_block` walks each Opaque slot's
// pinned concrete type and asks `solve_impl_in_ctx_with_args` whether
// the type satisfies each bound. For a body of `panic!(...)` (or
// `return`/`break`/etc.), the pinned type is `RType::Never` —
// `solve_impl` has no `Never` arm and doesn't model `!` as
// satisfying every trait, so validation errors with "RPIT body return
// type `!` does not satisfy bound `<Trait>`".
//
// Architectural shape: a "Never satisfies any trait" carve-out at
// the consumer (the per-slot bound-check loop) is the smaller move;
// the principled answer is the per-slot Var rule rt5#1 forced — an
// unbound Var after unify means the body produced no concrete
// constraint on this slot (diverging body, zero-iteration loop,
// future "no value flowed" cases), so the pin is vacuous. That rule
// subsumes Never specifically.
//
// Fix shape (landed): coerce/unify split (Phase 1) made `coerce(Never,
// Var)` short-circuit without binding. The pin loop's previous
// `pinned_rt == Never` branch is now an unbound-Var branch — when
// `subst.substitute(Var)` returns the Var unchanged, pin to `Never`
// and skip bound validation. One rule covers diverging bodies and
// any future no-value-flowed case.
#[test]
fn problem_1_rpit_diverging_body_rejected() {
    let bytes = try_compile_example(
        "redteaming/rt4/rpit_diverging_body",
        "lib.rs",
    )
    .expect("expected RPIT body of `!` to compile (validation should skip Never)");
    let _ = bytes;
}

// PROBLEM 2: A `where` clause on an `impl` block parses successfully
// but isn't merged into `impl_type_param_bounds`, so methods inside
// the impl don't see the bound. A method body that needs to call a
// trait method on `T` fails because `T`'s bound list is empty —
// even though the impl's where-clause says otherwise.
//
// Architectural shape: where-clause merge was wired for `Function`
// (Param-LHS merges into the fn's `type_param_bounds`), but
// `ImplBlock.where_clause` was added to the AST/parser without
// analogous typeck-side merging. Inside methods of such an impl
// the receiver-typed `T` reports no Required-bound and method
// dispatch fails.
//
// Fix shape: after resolving the impl's target in `collect_funcs`,
// walk `ib.where_clause`. Param-LHS preds extend
// `impl_type_param_bounds` (so `register_function` sees the bound on
// `T` for every method inside this impl). Complex-LHS preds attach
// to a new per-impl `where_predicates` field for call-time
// enforcement.
#[test]
fn problem_2_impl_where_clause_unenforced() {
    let bytes = try_compile_example(
        "redteaming/rt4/impl_where_unenforced",
        "lib.rs",
    )
    .expect("expected impl-where-clause to merge into impl bounds so method body sees `T: Required`");
    let _ = bytes;
}

// PROBLEM 3: An RPIT function that's CALLED before it's been body-
// checked (in declaration order within the same module) errors with
// "no method `<m>` on `impl <fn#slot>`". The root is that
// `finalize_rpit_for_one_function` only substitutes
// `Opaque{this_fn, slot} → pin` in the RPIT function's own
// `return_type` at the END of its body check. Callers earlier in the
// module have already had their bodies type-checked against the
// still-Opaque signature; the post-check finalize doesn't re-type
// them, so the call-site method dispatch on `Opaque{...}` fires
// against an `impl`-less type and fails.
//
// Architectural shape: post-hoc rewriting of stored types is
// inherently fragile — every new RType-holding table is a new place
// finalize has to remember to walk. rt5#6 demonstrated the pattern's
// failure mode (expr_types left out → codegen panic).
//
// Fix shape (landed): retire post-hoc rewriting entirely. `Opaque`
// becomes a stable indirection through typeck; mono and codegen
// peel via `peel_opaque(rt, &FuncTable)` at the conversion boundary
// (Phase 2). Method dispatch on `Opaque` receivers consults the
// slot's bounds directly via `check_method_call_opaque` — the
// "option (b)" of the original plan, but achieved by removing the
// alternative rather than choosing it. `finalize_rpit_substitutions`
// and friends are deleted.
#[test]
fn problem_3_rpit_forward_reference_fails() {
    let bytes = try_compile_example(
        "redteaming/rt4/rpit_forward_reference",
        "lib.rs",
    )
    .expect("expected forward reference to RPIT fn to compile");
    let _ = bytes;
}

// PROBLEM 4: A `where` clause on a trait method **declaration**
// parses but never enters the trait method's recorded type-param
// bound list. As a consequence, an impl method's body — which
// inherits the trait method's bounds — sees `T` with no Required
// bound and method dispatch on `T` fails.
//
// Architectural shape: `TraitMethodEntry` had no per-type-param
// bound storage. Inline `<T: Bound>` and where-clause `T: Bound`
// were both silently lost; impls couldn't inherit either form. The
// fix adds `TraitMethodEntry.type_param_bounds`, populates from
// inline + where-clause merge in `resolve_trait_methods`, and
// makes `register_function` for impl methods inherit the matching
// trait method's bounds onto the impl method's own slots.
#[test]
fn problem_4_trait_method_where_clause_dropped() {
    let bytes = try_compile_example(
        "redteaming/rt4/trait_method_where_dropped",
        "lib.rs",
    )
    .expect("expected trait-method where-clause to flow into impl's bound view of T");
    let _ = bytes;
}

// PROBLEM 5: APIT bare-call dispatch hardcodes the trait path to
// `std::ops::Fn` regardless of which Fn-family bound the type-param
// actually carries. `check_bare_typeparam_fn_call` builds a
// `PendingTraitDispatch` with `trait_path = vec!["std", "ops", "Fn"]`
// even when the caller's `<F: FnMut(...)>` only supplies `FnMut +
// FnOnce`. Since `Fn` is more restrictive than `FnMut`, the dispatch
// fails impl-resolution: typeck/codegen looks for a `Fn`-row that
// doesn't exist (the synthesized closure's mutating body only emits
// `FnMut + FnOnce`), and the call errors at the impl-resolution step.
//
// Architectural shape: bare-call sugar for `f(args)` should pick
// the most-restrictive Fn-family trait the param actually impls
// (FnOnce-only → call_once, FnMut → call_mut, Fn → call).
// `typeparam_fn_signature` already inspects the matching bound's
// path; the dispatch should record that trait, not hardcoded `Fn`.
//
// Fix shape: thread the bound's resolved trait path out of
// `typeparam_fn_signature` and use it as the dispatch's `trait_path`.
// Today the function discards the path and only returns
// `(param_types, return_type)`. Same fix probably needs to flow into
// the synthesized method name (`call` / `call_mut` / `call_once`).
#[test]
fn problem_5_apit_barecall_hardcoded_to_fn() {
    let bytes = try_compile_example(
        "redteaming/rt4/apit_barecall_fnmut",
        "lib.rs",
    )
    .expect("expected `f(args)` against an FnMut-bounded param to compile");
    let _ = bytes;
}

// PROBLEM 6: A `where` clause whose LHS is a lifetime
// (`where 'a: 'b`) errors at the parser with a confusing diagnostic
// because `parse_where_clause_opt` calls `parse_type` for the LHS,
// and `parse_type` doesn't accept a lifetime token. Real Rust
// supports lifetime-on-lifetime predicates as the canonical way to
// spell outlives obligations.
//
// Architectural shape: `parse_where_clause_opt` collapsed the
// "predicate LHS" into "type expression LHS" — but Rust's grammar
// distinguishes type-bounds (`T: Trait`) from lifetime-bounds
// (`'a: 'b`). The latter has its own LHS shape (a Lifetime), not a
// Type.
//
// Fix shape: peek the first token of each predicate. If it's a
// lifetime, consume `'a`, expect `:`, parse a `+`-list of lifetimes,
// and store on a separate `lifetime_predicates` vec on whatever AST
// nodes carry the where-clause. (Pocket-rust's lifetime checking is
// Phase B structural-only, so the predicates can be carry-only at
// first; semantic enforcement comes later.)
#[test]
fn problem_6_where_lifetime_predicate_parses() {
    let bytes = try_compile_example(
        "redteaming/rt4/where_lifetime_predicate",
        "lib.rs",
    )
    .expect("expected `where 'a: 'b` predicate to parse and be carried");
    let _ = bytes;
}

// PROBLEM 7: RPIT in a trait method **declaration** (`trait Foo {
// fn x() -> impl Bar; }`) errors at trait setup. `resolve_trait_methods`
// resolves each method's return type via the plain `resolve_type` —
// which rejects `TypeKind::ImplTrait` with "only allowed in argument
// position". The RPIT-aware rewrite (`rewrite_rpit_in_type` plus the
// post-resolve `Param → Opaque` substitution) is wired only into
// `register_function`, which trait method declarations don't go
// through.
//
// Architectural shape: trait method-sig setup duplicates a chunk of
// register_function's logic but missed the RPIT path. Three forks
// of "resolve a method's return type" exist (trait sig, impl
// inherent method, impl trait method) — only one (impl methods,
// via register_function) wires RPIT.
//
// Fix shape: factor the RPIT rewrite into a shared helper used by
// every site that resolves a fn-return position. `resolve_trait_methods`
// gets the same helper. The trait entry's method record needs
// per-method `rpit_slots: Vec<RpitSlot>` parallel to
// `FnSymbol.rpit_slots`; impls of the trait must produce the same
// number of pinnable slots, with each impl's pin determined by its
// own method body.
#[test]
fn problem_7_rpit_in_trait_method_signature() {
    let bytes = try_compile_example(
        "redteaming/rt4/rpit_in_trait_method_sig",
        "lib.rs",
    )
    .expect("expected RPIT-in-trait-method-sig to parse and feed through trait setup");
    let _ = bytes;
}
