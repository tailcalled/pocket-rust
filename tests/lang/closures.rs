// Closure expressions: `|args| body`, `move |args| body`, `||` /
// `move ||` no-arg forms, and the `Fn(T) -> R` parenthesized trait
// sugar plus `for<'a>` HRTB syntax. Phase 1A scope: non-capturing
// closures with full param-type inference (annotations optional).
// Captures are rejected at typeck. `f(args)` call sugar isn't wired
// yet — closures can be declared and stored but the only way to
// invoke them is via the future call sugar.

use super::*;

// Non-capturing, fully-annotated closures compile cleanly: typeck
// infers param/return types, lowering emits a `__closure_<id>` unit
// struct + `impl Fn<...>`, and the resulting program exits via the
// crate root's `answer`.
#[test]
fn non_capturing_annotated_closure_compiles() {
    let _bytes = compile_inline(
        "fn answer() -> u32 { let _f = |x: u32| x + 1u32; 0u32 }",
    );
}

#[test]
fn move_closure_compiles() {
    let _bytes = compile_inline(
        "fn answer() -> u32 { let _f = move |x: u32| x + 1u32; 0u32 }",
    );
}

#[test]
fn no_arg_closure_compiles() {
    let _bytes = compile_inline("fn answer() -> u32 { let _f = || 7u32; 0u32 }");
}

#[test]
fn move_no_arg_closure_compiles() {
    let _bytes = compile_inline("fn answer() -> u32 { let _f = move || 7u32; 0u32 }");
}

#[test]
fn typed_closure_param_compiles() {
    let _bytes = compile_inline("fn answer() -> u32 { let _f = |x: u32| x; 0u32 }");
}

#[test]
fn closure_with_explicit_return_type_compiles() {
    let _bytes = compile_inline(
        "fn answer() -> u32 { let _f = |x: u32| -> u32 { x + 1u32 }; 0u32 }",
    );
}

#[test]
fn multi_param_closure_compiles() {
    let _bytes = compile_inline("fn answer() -> u32 { let _f = |a: u32, b: u32| a; 0u32 }");
}

// Identity body works without param annotations because no operations
// are needed on the param's unbound inference var. Type-arg inference
// from a call site (e.g. inferring `x: u32` from `apply(|x| x)`)
// requires bidirectional inference and remains open work.
#[test]
fn unannotated_identity_closure_compiles() {
    let _bytes = compile_inline("fn answer() -> u32 { let _f = |x: u32| x; 0u32 }");
}

#[test]
fn closure_with_explicit_return_requires_brace_body() {
    let err = compile_source(
        "fn answer() -> u32 { let f = |x: u32| -> u32 x + 1; 0 }",
    );
    assert!(
        err.contains("closure body must be a `{ … }` block"),
        "expected brace-body error, got: {}",
        err,
    );
}

// Phase 2A: closures capture Copy outer bindings by-value. Each
// capture becomes a field on the synthesized struct; in the body, the
// captured name resolves via `self.<name>` field access. Runtime test
// confirms the captured value flows through the struct + impl.
#[test]
fn closure_captures_outer_copy_binding() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let outer = 5u32; let f = |x: u32| x + outer; f.call((10u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Phase 3G: `&mut Var(captured)` borrow detection. Taking a mut
// borrow of a captured binding marks the closure as mutating →
// FnMut-only synthesis. Verified by closure body that takes `&mut
// counter`, mutates through it via a helper.
fn helper_mutates(c: &mut u32) {
    *c = *c + 1u32;
}
#[test]
fn closure_mut_borrow_of_capture_triggers_fn_mut() {
    // Body uses `&mut counter` to pass to a helper. The borrow itself
    // upgrades the capture mode; the helper-call mutates through it.
    // Without the upgrade, Fn would be synthesized and the body's
    // `&mut counter` would fail at the &mut-place check.
    let bytes = compile_inline(
        "fn helper(c: &mut u32) { *c = *c + 1u32; }\n\
         pub fn answer() -> u32 { let mut counter = 0u32; \
             let mut bumper = |unit: ()| helper(&mut counter); \
             bumper.call_mut(((),)); \
             bumper.call_mut(((),)); \
             counter }",
    );
    let _ = helper_mutates;
    assert_eq!(answer_u32(&bytes), 2);
}

// Phase 3F: compound-assign mutation through method dispatch. `x += y`
// desugars to `x.add_assign(y)` — a method call with `BorrowMut`
// recv_adjust. When the method receiver is a captured Var, the
// closure's body is detected as mutating, drives FnMut-only
// synthesis (no Fn).
#[test]
fn closure_compound_assign_triggers_fn_mut() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let mut counter = 0u32; \
             let mut incr = |x: u32| { counter += x; counter }; \
             incr.call_mut((3u32,)); \
             incr.call_mut((4u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

// Phase 3E: FnMut detection. A non-`move` closure whose body
// mutates a captured binding gets only FnMut + FnOnce impls (no
// Fn). Caller must use `let mut f` so `f.call_mut(...)` autorefs
// to `&mut f`. The capture's struct field is `&'cap mut T` for non-
// Copy types, or by-value Move for Copy types (mutated through
// `&mut self`).
#[test]
fn closure_mutating_copy_capture_dispatches_fn_mut() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let mut counter = 0u32; \
             let mut incr = |x: u32| { counter = counter + x; counter }; \
             incr.call_mut((3u32,)); \
             incr.call_mut((4u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

// Bare-call sugar with mutating closure: `f(args)` dispatches via
// Fn::call which is now SKIPPED for mutating closures. Without `Fn`
// impl, `f(args)` must error or fall through. Currently typeck still
// records it as a bare-closure-call routing to Fn::call — codegen
// then fails because no Fn impl exists. Pin this as a follow-up.
// Until then, mutating closures must use explicit `.call_mut(...)`.
#[test]
fn closure_mutating_must_use_call_mut() {
    // `let mut f = |...| { mutate }; f.call_mut(...)` works (above).
    // Bare `f(args)` would fall through to Fn::call which isn't
    // synthesized — verify we still produce a valid module here by
    // sticking with explicit call_mut.
    let _bytes = compile_inline(
        "pub fn answer() -> u32 { let mut counter = 0u32; \
             let mut incr = |x: u32| { counter = counter + x; counter }; \
             incr.call_mut((5u32,)) }",
    );
}

// Phase 3C: bare `f(args)` call sugar. Typeck detects when a Call's
// callee resolves to a local of closure type and dispatches as
// `local.call((args,))` — recorded on `bare_closure_calls` for mono
// to lower as a MethodCall MonoExpr.
#[test]
fn closure_bare_call_returns_expected_value() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |x: u32| x + 1u32; f(5u32) }",
    );
    assert_eq!(answer_u32(&bytes), 6);
}

#[test]
fn closure_bare_no_arg_call() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = || 7u32; f() }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

#[test]
fn closure_bare_multi_arg_call() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |a: u32, b: u32| a + b; f(10u32, 5u32) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Bare call captures: outer binding referenced inside body.
#[test]
fn closure_bare_call_with_capture() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let outer = 5u32; let f = |x: u32| x + outer; f(10u32) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Phase 3C: bidirectional inference. When a closure expression is
// passed to a function whose parameter has a `Fn(A) -> R` bound, the
// closure's unannotated params/return get pre-unified with the
// bound's args/output BEFORE the body is checked. Lifts the
// "unannotated closure params must be numeric" restriction.
#[test]
fn closure_arg_type_inferred_from_fn_bound() {
    let bytes = compile_inline(
        "fn apply<F: Fn(u32) -> u32>(f: F) -> u32 { f.call((5u32,)) }\n\
         pub fn answer() -> u32 { apply(|x| x + 1u32) }",
    );
    assert_eq!(answer_u32(&bytes), 6);
}

// `apply` taking `Fn(&u32) -> u32` propagates `&u32` into the
// closure's `x` param; body uses `*x` to deref.
#[test]
fn closure_arg_type_inferred_handles_ref() {
    let bytes = compile_inline(
        "fn apply<F: Fn(&u32) -> u32>(f: F) -> u32 { f.call((&5u32,)) }\n\
         pub fn answer() -> u32 { apply(|x| *x + 1u32) }",
    );
    assert_eq!(answer_u32(&bytes), 6);
}

// Phase 3B: non-`move` closures synthesize Fn + FnMut + FnOnce impls
// (with the Fn:FnMut:FnOnce supertrait chain restored). Each method
// is callable on the same closure value, exercising all three impl
// rows. `call` returns the value, then `call_mut` and `call_once` on
// fresh closure values do the same — runtime confirms each impl
// produces the right answer.
#[test]
fn closure_call_call_mut_call_once_all_dispatch() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { \
             let f1 = |x: u32| x + 1u32; \
             let mut f2 = |x: u32| x + 1u32; \
             let f3 = |x: u32| x + 1u32; \
             f1.call((10u32,)) + f2.call_mut((20u32,)) + f3.call_once((30u32,)) \
         }",
    );
    assert_eq!(answer_u32(&bytes), 11 + 21 + 31);
}

// Phase 3A: `move` keyword forces by-value capture even for non-Copy
// types and synthesizes `FnOnce` instead of `Fn`. The closure consumes
// the outer binding into the struct field, then `call_once` consumes
// the closure on invocation. Runtime test exercises owned-Vec capture.
#[test]
fn move_closure_consumes_non_copy_capture() {
    let bytes = compile_inline(
        "struct Owned { v: u32 }\n\
         pub fn answer() -> u32 { let outer = Owned { v: 9u32 }; \
             let f = move |x: u32| x + outer.v; \
             f.call_once((10u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 19);
}

// `move` with a Copy capture also lowers to FnOnce — the call site
// must use `.call_once`. Phase 3B will restore the supertrait chain
// so move-Fn closures get all three impls auto-synthesized.
#[test]
fn move_closure_with_copy_capture() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let outer = 5u32; \
             let f = move |x: u32| x + outer; \
             f.call_once((10u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Phase 2B: non-Copy captures stored as `&'cap T` fields. The struct
// gets a `'cap` lifetime param; the body's `Var(name)` rewrites to
// `*self.<name>` to get the place, so field accesses on the captured
// non-Copy value go through autoref. Closure expression site borrows
// the binding (`&outer`) into the field.
#[test]
fn closure_captures_non_copy_via_borrow() {
    let bytes = compile_inline(
        "struct NoCopy { v: u32 }\n\
         pub fn answer() -> u32 { let outer = NoCopy { v: 7u32 }; \
             let f = |x: u32| x + outer.v; \
             f.call((10u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 17);
}

// Direct method-call dispatch on a closure value via the synthesized
// `Fn::call` impl. Typeck routes through the closure-records side
// table to populate `MethodResolution.trait_dispatch`; codegen then
// resolves the impl row at emit time (after `closure_lower` has
// registered the synth `impl Fn<...> for __closure_<id>`).
#[test]
fn closure_invoked_via_call_method_compiles() {
    let _bytes = compile_inline(
        "fn answer() -> u32 { let f = |x: u32| x + 1u32; f.call((5u32,)) }",
    );
}

// Round-trip: closure compiles AND the call returns the expected
// value at runtime. `f.call((5,))` should produce 6.
#[test]
fn closure_call_returns_expected_value() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |x: u32| x + 1u32; f.call((5u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 6);
}

// No-arg closure: `f.call(())` returns 7.
#[test]
fn no_arg_closure_call_returns_expected_value() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = || 7u32; f.call(()) }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

// Multi-param closure: `f.call((10, 5))` returns 15.
#[test]
fn multi_param_closure_call_returns_expected_value() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |a: u32, b: u32| a + b; f.call((10u32, 5u32)) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Unannotated closure params dispatch through the num-lit path: x's
// type is inferred from the call-site arg (5u32), the body's `x + 1`
// dispatches Add via the num-lit fallback, integer-class var unifies
// with u32 at the call site.
#[test]
fn unannotated_closure_param_inferred_from_call_site() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |x| x + 1u32; f.call((5u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 6);
}


// Parenthesized `Fn(T) -> R` sugar in TraitBound position.
#[test]
fn fn_trait_sugar_in_bound_parses() {
    let _bytes = compile_inline(
        "fn apply<F: Fn(u32) -> u32>(_f: F) -> u32 { 0u32 }\nfn answer() -> u32 { 0u32 }",
    );
}

// HRTB syntax parses.
#[test]
fn hrtb_in_bound_parses() {
    let _bytes = compile_inline(
        "fn apply<F: for<'a> Fn(&'a u32) -> u32>(_f: F) -> u32 { 0u32 }\nfn answer() -> u32 { 0u32 }",
    );
}

// HRTB lifetime is in scope inside the bound's args. The fn's
// lifetime_params is empty, so without HRTB the `'a` would be
// undeclared. With `for<'a>`, the bound's args validate.
#[test]
fn hrtb_lifetime_validates_via_bound_scope() {
    let bytes = compile_inline(
        "fn apply<F: for<'a> Fn(&'a u32) -> u32>(f: F) -> u32 { let x = 5u32; f.call((&x,)) }\n\
         pub fn answer() -> u32 { apply(|x: &u32| *x + 1u32) }",
    );
    assert_eq!(answer_u32(&bytes), 6);
}

// Multi-lifetime HRTB.
#[test]
fn hrtb_multi_lifetime() {
    let bytes = compile_inline(
        "fn apply<F: for<'a, 'b> Fn(&'a u32, &'b u32) -> u32>(f: F) -> u32 { \
             let x = 10u32; let y = 5u32; f.call((&x, &y)) }\n\
         pub fn answer() -> u32 { apply(|a: &u32, b: &u32| *a + *b) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Without HRTB, `'a` is undeclared and the bound is rejected.
#[test]
fn bound_with_undeclared_lifetime_rejected() {
    let err = compile_source(
        "fn apply<F: Fn(&'a u32) -> u32>(_f: F) -> u32 { 0u32 }\nfn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("undeclared lifetime"),
        "expected undeclared-lifetime error, got: {}",
        err,
    );
}

// Logical-or in expression position still works.
#[test]
fn logical_or_still_works() {
    expect_answer("lang/operators/logical_or", 42i32);
}

// Tuple-pattern parameter: `|(a, b)| a + b`. Closure_lower lays
// the param out as `let (a, b) = __args.0;` in the synthesized
// `Fn::call` body, so the existing pattern-binding pipeline binds
// `a` and `b`.
#[test]
fn tuple_pattern_closure_param_returns_15() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |(a, b): (u32, u32)| a + b; f.call(((10u32, 5u32),)) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Wildcard parameter: `|_| 42`. The synthesized prelude becomes
// `let _ = __args.0;` — the existing wildcard pattern path
// evaluates the value for side effects (none here) and drops it.
#[test]
fn wildcard_closure_param_returns_42() {
    let bytes = compile_inline(
        "pub fn answer() -> u32 { let f = |_: u32| 42u32; f.call((99u32,)) }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

// Negative: refutable pattern (integer literal) in a closure
// parameter is rejected. Same machinery as let-binding refutability:
// `pattern_is_irrefutable` against the (concrete) param type.
#[test]
fn refutable_closure_pattern_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let f = |0: u32| 1u32; f.call((0u32,)) }",
    );
    assert!(
        err.contains("refutable pattern in closure"),
        "expected refutable-pattern error, got: {}",
        err,
    );
}

// Argument-position `impl Trait`: `fn apply(f: impl Fn(u32) -> u32)`
// desugars at parse time to an anonymous type-param
// `<__impl_0: Fn(u32) -> u32>` bound on the function. Bidirectional
// inference still flows the closure's expected param/return through
// the bound.
#[test]
fn apit_with_fn_bound_returns_15() {
    let bytes = compile_inline(
        "fn apply(f: impl Fn(u32) -> u32) -> u32 { f.call((10u32,)) }\n\
         pub fn answer() -> u32 { apply(|x| x + 5u32) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// APIT works with the bare-call sugar too — the synthesized type-param
// participates in the same Fn-bound dispatch.
#[test]
fn apit_with_fn_bound_bare_call_returns_8() {
    let bytes = compile_inline(
        "fn apply(f: impl Fn(u32) -> u32) -> u32 { f(3u32) }\n\
         pub fn answer() -> u32 { apply(|x| x + 5u32) }",
    );
    assert_eq!(answer_u32(&bytes), 8);
}

// Negative: `impl Trait` in return position is not yet supported. The
// parser produces a `TypeKind::ImplTrait` that survives into typeck;
// `resolve_type` rejects it with a clear diagnostic.
#[test]
fn return_position_impl_trait_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         fn make() -> impl Show { 0u32 }\n\
         fn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("`impl Trait` is only allowed in argument position"),
        "expected APIT-only error, got: {}",
        err,
    );
}

// Negative: `impl Trait` in a struct field is also rejected.
#[test]
fn impl_trait_in_struct_field_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Holder { x: impl Show }\n\
         fn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("`impl Trait` is only allowed in argument position"),
        "expected APIT-only error, got: {}",
        err,
    );
}
