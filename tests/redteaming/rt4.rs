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
// Architectural shape: `!` is uninhabited — any trait obligation is
// vacuously true, but `solve_impl_in_ctx_with_args` doesn't know that.
// The validation of an RPIT pin should short-circuit when the pinned
// type is `Never`. Adding a special case in the validation loop is
// straightforward; the deeper question is whether `solve_impl` itself
// should learn that `!` satisfies anything (cleaner, but touches
// every other call site).
//
// Fix shape: in the per-slot validation loop in `check_block`, skip
// the bound check when `pinned_rt` is `RType::Never`. Optional:
// extend `solve_impl` directly so the same skip helps any future
// caller that asks "does `!` impl X?".
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
// but is **never enforced**. `parse_impl_block` populates
// `ImplBlock.where_clause: Vec<WherePredicate>`, but no setup pass
// reads it. A predicate naming an impossible bound (e.g. `where T:
// MissingTrait`, where `MissingTrait` resolves but `T` doesn't impl
// it) is silently accepted, and the impl block compiles.
//
// Architectural shape: where-clause enforcement was wired for
// `Function` (Param-LHS merges into `type_param_bounds`; complex-LHS
// goes to `where_predicates` and is checked at call site or setup).
// `ImplBlock.where_clause` was added to the AST + parser at the same
// time but no analogous typeck-side path consumes it. Methods inside
// such an impl don't see the predicate either.
//
// Fix shape: after resolving the impl's target, walk
// `ib.where_clause` the same way `register_function` does its
// function-level walk. Param-LHS preds extend `impl_type_param_bounds`
// (so methods inside see the bound on `T`); complex-LHS preds attach
// to a new `TraitImplEntry.where_predicates` field and get checked
// either at impl-validation time (concrete LHS, statically resolvable)
// or at every call site that uses this impl (LHS depends on impl-
// generic params).
#[test]
fn problem_2_impl_where_clause_unenforced() {
    let err = try_compile_example(
        "redteaming/rt4/impl_where_unenforced",
        "lib.rs",
    )
    .err()
    .expect("expected impl-level where-clause violation to be rejected");
    assert!(
        err.contains("where-clause") || err.contains("not satisfied") || err.contains("does not implement"),
        "expected impl-where-clause diagnostic, got: {}",
        err,
    );
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
// Architectural shape: typeck order is single-pass declaration
// order. Forward references work for ordinary fns because their
// `FnSymbol.return_type` is set at setup time (before any body is
// checked). RPIT functions defer their concrete return type to body
// check, which is too late for callers checked earlier in the same
// module.
//
// Fix shape: either (a) do a topological sort that body-checks RPIT
// fns first, (b) trait-dispatch on `Opaque{fn, slot}` consults the
// slot's `bounds` directly so method calls work even before the pin
// is set, or (c) two-pass: collect all RPIT pins in a pre-pass that
// body-checks each RPIT fn, then run regular body checks. Option (b)
// is the cleanest — opacity is preserved and ordering doesn't
// matter — but requires plumbing FuncTable through `solve_impl*`.
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
// parses successfully but is silently dropped. `parse_trait_method_sig`
// populates `TraitMethodSig.where_clause`, but `resolve_trait_methods`
// in setup walks each method, resolves its params + return type, and
// stores the signature on the trait entry — without ever consulting
// `where_clause`. A method declared with `fn x<T>() where T: Required`
// loses its `T: Required` bound: implementations don't have to honor
// it, and call sites don't enforce it.
//
// Architectural shape: trait-side method-sig setup is parallel to
// fn-side setup in `register_function` but doesn't share the where-
// clause processing logic. The merge-Param-LHS / store-complex-LHS
// pass exists only for `Function`. Methods declared inside trait
// bodies need an equivalent pass.
//
// Fix shape: add a where-clause processing block to
// `resolve_trait_methods` that mirrors `register_function`'s logic.
// Param-LHS preds get merged into the method's `type_param_bounds`;
// complex-LHS preds go on the trait entry's per-method
// `where_predicates` (new field) for call-site enforcement.
#[test]
fn problem_4_trait_method_where_clause_dropped() {
    let err = try_compile_example(
        "redteaming/rt4/trait_method_where_dropped",
        "lib.rs",
    )
    .err()
    .expect("expected trait-method where-clause violation at impl site to be rejected");
    assert!(
        err.contains("where-clause")
            || err.contains("not satisfied")
            || err.contains("does not implement")
            || err.contains("Required"),
        "expected trait-method-where-clause diagnostic, got: {}",
        err,
    );
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
