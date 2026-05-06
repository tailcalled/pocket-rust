// Round 3 of red-team findings — architectural bugs in the closure
// pipeline. Each test below documents one bug; **the test is
// expected to fail today** and the failure *is* the surfaced bug.
//
// rt3 differs from rt1/rt2 in scope: every problem here is rooted in
// closure handling (typeck/closure_lower/mono interactions) added in
// the recent closure work. The rt2 patterns ("invalid program
// accepted" / "valid program rejected") apply equally; each test's
// docstring names the architectural shape so a fix can target the
// right layer rather than patching the symptom.

use super::*;

// PROBLEM 1: bare-call sugar in `check_call` only intercepts when the
// callee resolves to a local of synthesized closure type. A local of
// any other type that happens to share its name with a function in
// scope falls through and pocket-rust calls THE FUNCTION. Real Rust
// rejects this with E0618 "expected function, found u32" because the
// local shadows the function in scope.
//
// Architectural shape: pocket-rust's resolution order in `check_call`
// is "function-table first, local check second". The bare-closure-
// call path was added at the top of `check_call` but only for
// closure-typed locals; non-closure locals still cede to the function
// lookup. Resolution should always prefer the local over the
// function entry — fn-table lookup should run only when no local
// with that name exists.
//
// Fix shape: factor the local-name lookup out of the bare-closure
// path. When `path.len() == 1` and the name resolves to ANY local,
// route based on the local's type: closure → bare-closure dispatch;
// fn-pointer → call-via-pointer (future); anything else → error
// "expected function, found <ty>". Only when no local exists should
// the function-table lookup fire.
#[test]
fn problem_1_local_shadows_fn_in_bare_call() {
    let err = try_compile_example(
        "redteaming/rt3/local_shadows_fn_in_bare_call",
        "lib.rs",
    )
    .err()
    .expect("expected compile error: local of u32 type is not callable");
    assert!(
        err.contains("expected function") || err.contains("not callable"),
        "expected fn-shadowing diagnostic, got: {}",
        err,
    );
}

// PROBLEM 2: `closure_lower::clone_expr_fresh_ids` rewrites
// `Var(captured_name)` → `self.<name>` lexically — every Var matching
// a capture name gets rewritten, with no scope tracking. When the
// closure body shadows a captured name with an inner `let`, the
// rewrite still fires for the inner reference and silently swaps in
// the captured value where the user's source said "use the inner
// local".
//
// Architectural shape: capture-rewrite is purely lexical against the
// captures' name set. Fix needs scope tracking inside
// `clone_expr_fresh_ids` — track which names are introduced by
// inner let-statements / inner closure params / pattern bindings,
// and skip the rewrite for shadowed Vars. Equivalent to running a
// small name-resolution pass over the cloned body, scoped on
// `Block`/`Match`-arm/`Closure`-param boundaries.
#[test]
fn problem_2_inner_let_shadow_of_captured_name() {
    expect_answer(
        "redteaming/rt3/closure_inner_shadows_capture",
        1005u32,
    );
}

// PROBLEM 3: closure capture detection lives only in
// `check_expr_inner`'s `ExprKind::Var` arm — when the lookup crosses
// a `closure_scopes` barrier, it records the capture. There's a
// SEPARATE Var lookup in `check_place_inner` (used when a Var
// appears in place position: as a method-call receiver, as the inner
// of a `&` / `&mut` borrow, as the base of a non-FieldAccess Deref
// chain, as the LHS of an assignment) that consults `ctx.locals`
// directly and never records captures. So a closure body that uses
// a captured binding in those positions never gets the capture
// recorded — synthesis sees zero captures, the synthesized struct
// is a unit struct, and the lifted method body errors with "unknown
// variable" because the rewrite didn't fire.
//
// Architectural shape: binding resolution happens at TWO sites with
// duplicated code paths. The fix is a single helper —
// `lookup_local_with_capture(ctx, name) -> Option<&LocalEntry>` —
// that walks `ctx.locals` AND records into the innermost crossed
// `closure_scopes` frame. Both `check_expr_inner` and
// `check_place_inner` would call it, eliminating the divergence.
//
// Without that, common closure idioms break: any closure body that
// calls a method on a captured binding (`outer.method()`), borrows
// it (`&outer`), or assigns through a deref of it (`*outer.field =
// ...`) silently fails to capture.
#[test]
fn problem_3_capture_in_place_position_not_recorded() {
    expect_answer(
        "redteaming/rt3/closure_capture_in_place_position",
        42u32,
    );
}

// PROBLEM 4: `closure_lower::rewrite_expr` processes child
// expressions before checking whether the visited node itself is a
// closure — but the Closure-rewrite path consumes `closure.body` via
// `std::mem::replace` without first descending into NESTED closures
// inside that body. The synthesized impl method's body comes from
// `clone_expr_fresh_ids(&closure.body, …)`, whose `Closure(_)` arm
// is `unreachable!("inner closures must be rewritten before
// clone_expr_fresh_ids")`. So a closure whose body contains a
// closure expression panics the compiler instead of producing a
// diagnostic or correct lowering.
//
// Architectural shape: rewriting is a single pre-order pass, but the
// Closure case at the bottom of `rewrite_expr` is structurally a
// LATE rewrite — it consumes the AST node — and the regular
// children-walk earlier in the function doesn't dive into the
// to-be-consumed `closure.body`. Two fix shapes:
//   (1) Before `std::mem::replace`-ing `expr`, recurse into
//       `closure.body` so nested closures get rewritten first;
//   (2) Make `clone_expr_fresh_ids` handle `Closure(_)` directly —
//       recursing, allocating IDs, returning a cloned Closure that a
//       subsequent walk picks up.
// (1) is simpler and matches how the rest of `closure_lower` already
// thinks about the traversal.
//
// Today the failure is a `panic!`, which `try_compile_example`
// converts to a panic propagation rather than an Err. So the test
// asserts the program compiles successfully (it can't), and the
// natural failure is the harness propagating the panic — visible as
// a test panic rather than a clean compile error.
#[test]
fn problem_4_nested_closures_panic() {
    expect_answer(
        "redteaming/rt3/nested_closures_panic",
        8u32,
    );
}

// PROBLEM 5: closures inside generic functions can't reference the
// enclosing fn's type-params. `check_closure` resolves the closure
// params' type annotations against `ctx.type_params` (which is the
// enclosing fn's type-param list), so the *initial* body typeck
// works. But the synthesized impl method registered by
// `register_synthesized_closure_impl` has no type-params — it's a
// concrete method on the concrete `__closure_<id>` struct. When
// `check_function` re-types the synthesized method's body (which
// contains the closure body cloned in), the enclosing fn's
// type-params aren't in scope and resolution fails with "unknown
// type: T".
//
// Architectural shape: `closure_lower` doesn't propagate the
// enclosing template's type-params to the synthesized struct + impl.
// For a closure inside `fn helper<T>(x: T)`, the synthesized struct
// should be `__closure_<id><T>` (carrying the same type-params), the
// impl should be `impl<T> Fn<(T,)> for __closure_<id><T>`, and the
// method body's `T` references resolve against the impl's type-
// params during synth-method `check_function`. ClosureInfo would
// gain an `enclosing_type_params: Vec<String>` field captured at
// typeck time, and synthesis wires it through.
//
// Impact: every generic-bearing closure use-case fails —
// `fn map<T, F: Fn(T) -> T>(…)` callers, generic helpers, any
// closure inside `src/typeck/` / `src/borrowck/` (most of pocket-
// rust's own helpers ARE generic) — so the `selfhost` target hits
// this immediately when closures land in the bootstrap source.
#[test]
fn problem_5_closure_in_generic_fn_unknown_type() {
    expect_answer(
        "redteaming/rt3/closure_in_generic_fn_unknown_type",
        42u32,
    );
}
