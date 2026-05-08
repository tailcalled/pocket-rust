// Round 5 of red-team findings — architectural bugs surfaced after
// the rt4 fixes landed. Each test below documents one bug; **the
// test is expected to fail today** and the failure *is* the surfaced
// bug. The fix shape names the layer where the change belongs.

use super::*;

// PROBLEM 1: An RPIT function whose body is *only* a diverging
// expression (`fn make() -> impl Show { panic!() }`) is rejected
// at validation. The body's tail type is `Never`. The slot's
// expected type is a fresh inference Var. `unify(Never, Var)`
// short-circuits (Never coerces to anything *without* binding the
// other side), so the Var remains unbound. When the per-slot pin
// validation reads it back via `infer_to_rtype_for_check`, the
// unbound Var defaults to `RType::Int(I32)` — and the validation
// errors "RPIT body return type `i32` does not satisfy bound
// `Show`", which is doubly wrong: the body's actual type is `!`,
// not `i32`, and `!` is uninhabited so any bound is vacuously
// true.
//
// Architectural shape: bundling Never-coercion into `unify` is a
// simplification (Rust separates `unify` from `coerce` — `coerce`
// has the Never-up rule, `unify` is invariant). The bundling means
// every consumer of an inference Var has to think about whether
// the Var was "really unconstrained" vs "unified-with-Never-as-no-
// op." rt4#1's Never short-circuit assumed the latter; the actual
// state is the former.
//
// Real Rust accepts an always-diverging RPIT body: `!` is
// uninhabited and the function type-checks at the abstract level.
//
// Fix shape (landed): split `coerce`/`unify` (Phase 1). `coerce(Never,
// _)` succeeds without binding (the value-flow rule); `unify` is
// uniformly invariant. The pin-validation loop interprets an
// unbound Var as "the body produced no concrete constraint" → pin
// to `Never` and skip the bound check. Vacuously true on a never-
// reached path; downstream consumers see a concrete `Never` rather
// than an unresolved Var. Phase 2's `peel_opaque` then resolves
// `Opaque{make,0}` to `Never` at the caller, and mono's trait
// dispatch short-circuits on Never receivers (returns import idx 0
// — the call is unreachable; wasm validator polymorphic post-
// `panic`'s `unreachable`).
#[test]
fn problem_1_rpit_diverging_only_body_rejected() {
    let bytes = try_compile_example(
        "redteaming/rt5/rpit_diverging_body_caller",
        "lib.rs",
    )
    .expect("expected caller of an RPIT fn whose body diverges to compile");
    let _ = bytes;
}

// PROBLEM 2: A trait method `where Self: Bound` is silently dropped.
// rt4#4 added merging of Param-LHS where-preds onto the trait
// method's `type_param_bounds`, but `Self` isn't a regular type-
// param — it's an implicit self_target — so the merge loop's
// `RType::Param(name)` lookup against `type_params` always misses
// for `Self`. The predicate goes nowhere; it isn't stored on the
// trait entry, isn't checked at impl-validation, isn't merged
// onto impl methods.
//
// Architectural shape: `Self`-LHS predicates need a different
// destination than per-type-param bound rows — they constrain the
// impl's *target type*, not a method-level type-param. The natural
// home is a per-method `self_bounds` (or per-trait `where_self`)
// on `TraitMethodEntry`, checked at impl validation time
// (`validate_trait_impl_signatures` resolves the impl's target
// against each Self-bound, errors if the impl target doesn't
// satisfy).
//
// Real Rust rejects an `impl Foo for NoBar` when Foo declares
// `fn x() where Self: Bar` and NoBar doesn't impl Bar.
#[test]
fn problem_2_trait_method_self_bound_dropped() {
    let err = try_compile_example(
        "redteaming/rt5/trait_method_self_bound_dropped",
        "lib.rs",
    )
    .err()
    .expect("expected `impl Foo for NoBar` to be rejected for missing `Self: Bar`");
    assert!(
        err.contains("Bar")
            || err.contains("not satisfied")
            || err.contains("does not implement"),
        "expected Self-bound diagnostic, got: {}",
        err,
    );
}

// PROBLEM 3: A type-param bounded by `FnOnce(...) -> R` can be called
// more than once — pocket-rust doesn't enforce the FnOnce
// once-only semantics. `apply<F: FnOnce()>(f: F) { f(); f(); }`
// compiles cleanly even though the second call is a use-after-move
// of `f`.
//
// Architectural shape: rt4#5 wired the dispatch's `recv_adjust =
// Move` for FnOnce, but borrowck's move-tracking on a Param-typed
// binding doesn't observe the moves through trait-dispatched
// method calls. Each `f(args)` lowers to a `MethodCall` MonoExpr
// whose receiver is `f` (move-out) — but the borrowck CFG records
// these as ordinary method calls, not as move sites on the binding.
//
// Real Rust rejects with E0382: "use of moved value `f`".
//
// Fix shape (landed, Phase 4): borrowck `lower_call` consults
// `bare_closure_calls[node_id]` (typeck records the binding name
// when routing a Call through bare-typeparam dispatch) and
// `method_resolutions[node_id].recv_adjust`. When set, it
// synthesizes a receiver Operand for the binding — `Move` for
// `recv_adjust = Move`, `Copy` for borrow/by-ref. The synthesized
// operand prepends to the call's arg list, so the move-out lands
// in the CFG at this statement; the standard move-state lattice
// catches `f(); f();` as use-after-move. Generic over trait
// identity: any future `recv_adjust = Move` on a Param-typed
// receiver gets the same treatment without an FnOnce-specific code
// path.
#[test]
fn problem_3_fnonce_called_multiple_times() {
    let err = try_compile_example(
        "redteaming/rt5/fnonce_called_twice",
        "lib.rs",
    )
    .err()
    .expect("expected second `f()` to error as use-after-move");
    assert!(
        err.contains("moved")
            || err.contains("already moved")
            || err.contains("use of moved value"),
        "expected use-after-move error on FnOnce-twice, got: {}",
        err,
    );
}

// PROBLEM 5: `where 'a: 'static` — a lifetime predicate that names
// the built-in `'static` lifetime — is rejected with "undeclared
// lifetime `'static` in where-clause". rt4#6's validation iterates
// the enclosing fn/impl's `lifetime_param_names` to check that
// every named lifetime in a predicate is in scope; `'static` is
// always in scope but isn't a user-declared parameter, so the loop
// misses it.
//
// Architectural shape: my validation conflated "declared as a
// `<'a>` parameter" with "in scope at this site". The two differ
// for the built-in lifetimes (`'static` today; `'_` is technically
// a placeholder, not a name, but we'd want the same handling for
// any future built-ins).
//
// Fix shape (landed, Phase L2): `crate::typeck::lifetime_in_scope(name,
// &fn_lifetimes)` predicate as the single source of truth. Three
// where-clause validation sites in setup.rs route through it. The
// borrowck region pass mirrors the rule: `'static` resolves to
// `STATIC_REGION` (RegionId 0) at every lookup, never pushed into
// `sig_named`.
#[test]
fn problem_5_where_static_lifetime_rejected() {
    let bytes = try_compile_example(
        "redteaming/rt5/where_lifetime_unenforced",
        "lib.rs",
    )
    .expect("expected `where 'a: 'static` to be accepted");
    let _ = bytes;
}

// PROBLEMS 7–9 share an architectural shape: rt4#6 made `'a: 'b`
// and `T: Trait + 'a` predicates parse and validate (in-scope
// names) at setup, and stores the resolved `LifetimePredResolved`
// on `FnSymbol/Template.lifetime_predicates`. Originally that
// storage was dead — borrowck did no outlives reasoning.
//
// Fix shape (landed, Phases L0–L5): full region inference in
// borrowck. Per-fn `RegionGraph` (`src/borrowck/cfg.rs`) populated
// at L1's `populate_signature_regions` (sig-named, sig-inferred,
// where-clause edges, `'static` outlives every other region).
// L3's `populate_body_constraints` walks the CFG and emits
// outlives constraints for body operations — assignments, returns,
// reborrows, calls (with callee free-region instantiation +
// where-clause edges + variance-aware flow). L4's
// `regions::solve` does Floyd-Warshall closure of declared facts
// and verifies each requirement whose endpoints are sig-fixed
// (body-fresh regions are treated as solver-pickable free vars
// — the loose-but-correct simplification).
//
// Closes #7 and #9. #8 still fails — it needs scope-bound modeling
// on body regions (R_inner is bounded by inner block; R_r extends
// past). The simple "body-fresh = free var" treatment passes the
// caller-side constraint chain trivially; real Rust catches it via
// region intervals over CFG points. A future extension to L3 would
// track each body-fresh region's "max scope" CFG-point set; the
// solver would then reject when a required outlives implies the
// body region must extend past its scope.

// PROBLEM 7: Function body that contradicts its own where-clause
// is silently accepted. The signature declares `'a: 'b`, but the
// body returns a `'b`-lifetimed reference where the signature
// promises `'a`. Real Rust rejects: the predicate doesn't help
// (it points the wrong way). Pocket-rust accepts because
// lifetime relations aren't checked at all.
#[test]
fn problem_7_where_outlives_violated_in_body() {
    let err = try_compile_example(
        "redteaming/rt5/where_outlives_violated_in_body",
        "lib.rs",
    )
    .err()
    .expect("expected lifetime mismatch in body to be rejected");
    assert!(
        err.contains("lifetime")
            || err.contains("outlives")
            || err.contains("does not live long enough"),
        "expected lifetime-mismatch diagnostic, got: {}",
        err,
    );
}

// PROBLEM 8: A caller violating the callee's `where 'a: 'b`
// predicate is silently accepted. The callee constrains `'a` to
// outlive `'b`; the caller passes a short-lived ref where `'a`
// gets inferred, with a longer-lived ref filling `'b` — opposite
// of what the predicate requires. Real Rust rejects the call.
// Pocket-rust accepts and the program reads through a dangling
// ref at runtime.
#[test]
fn problem_8_where_outlives_unenforced_at_call_site() {
    let err = try_compile_example(
        "redteaming/rt5/where_outlives_unenforced_at_call_site",
        "lib.rs",
    )
    .err()
    .expect("expected call-site outlives violation to be rejected");
    assert!(
        err.contains("lifetime")
            || err.contains("outlives")
            || err.contains("does not live long enough")
            || err.contains("borrowed value does not live long enough"),
        "expected call-site outlives diagnostic, got: {}",
        err,
    );
}

// PROBLEM 9: A function whose body needs `'a: 'b` to type-check
// is silently accepted even when the predicate is *missing*.
// Real Rust requires the user to write `where 'a: 'b` to make
// the body sound; pocket-rust accepts the body either way
// because borrowck doesn't relate lifetimes. This makes the
// `lifetime_predicates` field dead storage — its presence or
// absence has no observable effect.
#[test]
fn problem_9_where_outlives_required_but_missing() {
    let err = try_compile_example(
        "redteaming/rt5/where_outlives_required_but_missing",
        "lib.rs",
    )
    .err()
    .expect("expected missing `where 'a: 'b` to be rejected");
    assert!(
        err.contains("lifetime") || err.contains("outlives"),
        "expected missing-predicate diagnostic, got: {}",
        err,
    );
}

// PROBLEM 6: Forward-reference + let-bind an RPIT fn's result
// crashes codegen. rt4#3 fixed `make().show()` (direct chain) by
// routing method dispatch on `Opaque` through the slot bounds
// AND extending `finalize_rpit_substitutions` to also rewrite
// `MethodResolution.trait_dispatch.recv_type`. But the local
// binding's recorded type lives in `FnSymbol.expr_types`, which
// the finalize pass doesn't walk. So `let r = make(); r.show()`
// leaves `expr_types[r] = Opaque{make, 0}` after typeck, and
// codegen's layout helpers (`byte_size_of`, `flatten_rtype`,
// `collect_leaves`) hit the `Opaque` arm's `unreachable!()`.
//
// Architectural shape: post-hoc rewriting of stored types is an
// open-ended commitment — every new RType-holding table is a new
// place finalize has to walk. Adding `expr_types` to finalize fixes
// THIS test but doesn't fix the pattern; the next contributor
// adding a table re-introduces the bug.
//
// Fix shape (landed, Phase 2): retire post-hoc rewriting. Add
// `peel_opaque(rt, &FuncTable)` and `subst_and_peel(rt, env, funcs)`
// in `src/typeck/types.rs`; mono and codegen route every Param-
// substitution through `subst_and_peel` so codegen never sees an
// `Opaque`. `finalize_rpit_substitutions` and friends are deleted.
// `Opaque` becomes a stable indirection through typeck; new RType-
// holding tables get peeled automatically when they pass through
// the mono/codegen substitution boundary.
#[test]
fn problem_6_rpit_forward_local_binding_crashes_codegen() {
    let bytes = try_compile_example(
        "redteaming/rt5/rpit_local_binding_forward",
        "lib.rs",
    )
    .expect("expected forward-reference + let-bind RPIT result to compile");
    let _ = bytes;
}

// PROBLEM 4: `impl<...> Holder<...> where <Complex>: Bound` — a
// complex-LHS where-clause on an impl block — is silently dropped.
// rt4#2 added merging for Param-LHS predicates into
// `impl_type_param_bounds`; complex-LHS preds (anything not
// `RType::Param(name)` of an impl-level type-param) fall off the
// match and don't go anywhere. No storage on the impl, no check
// at setup time even when the LHS is fully concrete and the
// predicate is statically false.
//
// Architectural shape: register_function for plain fns has a
// `where_predicates` list on the FnSymbol/Template plus a setup-
// time check for non-generic complex-LHS preds. Impl blocks need
// the analogue: per-impl `where_predicates` storage, and a
// concrete-LHS check at impl-declaration time.
//
// Real Rust rejects an `impl Holder<u32> where (u32,): MissingTrait`
// at the impl declaration itself, because the predicate is
// statically false.
#[test]
fn problem_4_impl_complex_where_clause_dropped() {
    let err = try_compile_example(
        "redteaming/rt5/impl_complex_where_dropped",
        "lib.rs",
    )
    .err()
    .expect("expected impl with statically-false where-clause to be rejected");
    assert!(
        err.contains("MissingTrait")
            || err.contains("not satisfied")
            || err.contains("where-clause"),
        "expected impl complex-LHS where-clause diagnostic, got: {}",
        err,
    );
}
