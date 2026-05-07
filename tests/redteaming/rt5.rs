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
// Architectural shape: `unify`'s Never-coerces-to-anything rule
// is correct for inference, but it has a side effect at the RPIT
// pin site that we didn't notice — the slot's Var doesn't get
// bound, so the pin lookup falls back to the int-default. rt4#1
// correctly handles the `pinned_rt == Never` case but never
// triggers because the substitute step doesn't reach Never; the
// Var stays a Var.
//
// Real Rust accepts an always-diverging RPIT body: `!` is
// uninhabited and the function type-checks at the abstract level.
//
// Fix shape: in the per-slot pin logic, when `resolved` is still
// `InferType::Var(_)` after substitute AND the body's tail type
// is `Never`, treat the pin as `Never` (or bind the Var to Never
// before validation). The unify rule itself is fine; the RPIT
// pin reader needs to recognize the "never coerced via the empty
// rule" case.
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
// Fix shape: when borrowck sees a method call dispatched as
// `FnOnce::call_once` (`recv_adjust = Move` on a Param-typed
// receiver), record the receiver's binding as moved at that
// CfgStmt. The standard move-state lattice then catches the
// second call as a use-after-move.
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
// Fix shape: pre-pop `'static` into the lifetime_param_names slice
// my where-clause validator consults (or short-circuit the
// validator when the name is `static`). Same handling applies to
// the trailing `+ 'lifetime` slot on type predicates.
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
// on `FnSymbol/Template.lifetime_predicates`. **No consumer ever
// reads that storage.** Borrowck is "Phase B structural-only" —
// it doesn't solve outlives obligations — so the predicates have
// zero effect on type-checking. Programs that should depend on
// them to be sound (or unsound) compile either way.
//
// Each of #7–9 demonstrates a different angle on the gap. The
// fix shape is shared: borrowck needs to consume
// `lifetime_predicates` as declared facts about the in-scope
// lifetimes, and either (a) check the function's body against the
// resulting outlives relations, or (b) check call-site
// substitutions against the callee's predicates.

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
// Architectural shape: the finalize substitution is incomplete —
// it walks return_types and method_resolutions, but not the
// per-NodeId `expr_types` table where binding/expression types
// are recorded. Any post-typeck pass that reads `expr_types`
// (mono storage layout, codegen leaf collection) sees `Opaque`
// for forward-referenced RPIT-bound locals.
//
// Fix shape: walk every `FnSymbol.expr_types` and `Template.expr_types`
// in `finalize_rpit_substitutions`, substituting each `Opaque{fn,
// slot} → pin` the same way return_types and trait_dispatches are
// handled.
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
