// `&` and `&mut` references, lifetimes, and NLL.
//
// Borrow-check *conflicts* (mutable+anything, write-while-borrowed,
// etc.) live in `tests/lang/borrowck.rs`. This file covers the
// positive-shape cases: ref codegen, deref, lifetime annotations,
// and NLL liveness scope shrinkage.

use super::*;

#[test]
fn borrows_returns_40() {
    expect_answer("lang/references/borrows", 40i32);
}

#[test]
fn escaping_borrow_returns_42() {
    expect_answer("lang/references/escaping_borrow", 42i32);
}

#[test]
fn mut_ref_through_binding_returns_99() {
    expect_answer("lang/references/mut_ref_through_binding", 99i32);
}

#[test]
fn mut_ref_direct_returns_50() {
    expect_answer("lang/references/mut_ref_direct", 50i32);
}

#[test]
fn mut_ref_field_returns_77() {
    expect_answer("lang/references/mut_ref_field", 77i32);
}

#[test]
fn inner_borrow_lifetime_returns_5() {
    expect_answer("lang/references/inner_borrow_lifetime", 5i32);
}

#[test]
fn borrow_field_returns_42() {
    expect_answer("lang/references/borrow_field", 42i32);
}

#[test]
fn place_borrow_noncopy_field_returns_7() {
    expect_answer("lang/references/place_borrow_noncopy_field", 7i32);
}

#[test]
fn place_borrow_through_ref_returns_42() {
    expect_answer("lang/references/place_borrow_through_ref", 42i32);
}

#[test]
fn nll_sequential_borrows_returns_7() {
    expect_answer("lang/references/nll_sequential_borrows", 7i32);
}

#[test]
fn nll_borrow_then_move_returns_7() {
    expect_answer("lang/references/nll_borrow_then_move", 7i32);
}

// Named lifetimes on functions tie param to return type. `pick_first<'a>`
// picks `x`'s lifetime; the elided y arg gets a fresh inferred one and
// doesn't constrain the result.
#[test]
fn lifetime_named_returns_42() {
    expect_answer("lang/references/lifetime_named", 42i32);
}

// Refs in struct fields: a generic `Wrapper<'a>` holds `&'a Inner` and a
// field-access produces the held borrow.
#[test]
fn lifetime_struct_field_returns_42() {
    expect_answer("lang/references/lifetime_struct_field", 42i32);
}

// `&'a self` receiver tied to the impl's lifetime param routes the
// receiver's borrow into the return ref.
#[test]
fn lifetime_self_receiver_returns_42() {
    expect_answer("lang/references/lifetime_self_receiver", 42i32);
}

// Two ref params share `'a`; the result borrows both.
#[test]
fn lifetime_combined_returns_42() {
    expect_answer("lang/references/lifetime_combined", 42i32);
}

// Anonymous `'_` lifetime: `'_` parses to a fresh `Inferred(0)`
// placeholder per occurrence (rather than a regular `Named("_")`), so
// it works in let-binding annotations and impl targets without users
// having to invent a unique `'a` name. Here `impl Drop for Logger<'_>`
// and `let _l: Logger<'_> = ...` both rely on it.
#[test]
fn anon_lifetime_impl_returns_42() {
    expect_answer("lang/references/anon_lifetime_impl", 42i32);
}

// Nested per-slot field borrow tracking. Reading `o.i.r` follows a
// multi-segment field path through a struct containing a struct-with-
// ref. Borrow propagation in cfg_borrows duplicates the inner borrow
// onto the outer holder; reads through that path return the borrow
// correctly.
#[test]
fn nested_field_borrow_returns_42() {
    expect_answer("lang/references/nested_field_borrow", 42i32);
}

#[test]
fn explicit_deref_through_shared_ref_returns_5() {
    // `*r` where `r: &u32` is *not* unsafe — it's autoderef written
    // explicitly.
    let bytes = compile_inline(
        "fn answer() -> u32 { let x: u32 = 5; let r: &u32 = &x; *r }",
    );
    assert_eq!(answer_u32(&bytes), 5);
}

#[test]
fn explicit_deref_through_mut_ref_writes_back() {
    // Whole-place assignment via `*r = …;`.
    let bytes = compile_inline(
        "fn answer() -> u32 { let mut x: u32 = 1; let r: &mut u32 = &mut x; *r = 42; x }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

#[test]
fn three_mut_calls_in_sequence_returns_3() {
    // Repeated `&mut` borrows of the same place across sequential
    // function calls — regression check for the real-pointer codegen
    // not regressing on this pattern.
    let bytes = compile_inline(
        "struct Counter { n: u32 } \
         fn set(c: &mut Counter, v: u32) -> u32 { c.n = v; c.n } \
         fn answer() -> u32 { \
             let mut c = Counter { n: 0 }; \
             let _a = set(&mut c, 1); \
             let _b = set(&mut c, 2); \
             let _z = set(&mut c, 3); \
             c.n \
         }",
    );
    assert_eq!(answer_u32(&bytes), 3);
}

#[test]
fn ref_in_struct_field_without_lifetime_is_rejected() {
    // Refs in struct fields require explicit lifetimes declared on the
    // struct.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Bad { p: &Point }",
    );
    assert!(
        err.contains("missing lifetime specifier"),
        "expected missing-lifetime-specifier error, got: {}",
        err
    );
}

#[test]
fn struct_field_ref_without_lifetime_is_rejected_already_listed() {
    // Spot-check that the same shape without an explicit `'a` errors
    // for typed struct fields too.
    let err = compile_source(
        "struct Inner { x: u32 }\nstruct Bad { p: &Inner }",
    );
    assert!(
        err.contains("missing lifetime specifier"),
        "expected missing-lifetime error, got: {}",
        err
    );
}

#[test]
fn ref_return_with_zero_ref_params_is_rejected() {
    // Lifetime elision rule 2 only kicks in with exactly one ref
    // param; zero ref params + ref return has no source lifetime.
    let err = compile_source(
        "fn whoops() -> &u32 { let x: u32 = 1; &x }",
    );
    assert!(
        err.contains("exactly one reference parameter"),
        "expected zero-ref-params error, got: {}",
        err
    );
}

#[test]
fn ref_return_with_two_ref_params_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn whoops(a: &Point, b: &Point) -> &Point { a }",
    );
    assert!(
        err.contains("exactly one reference parameter"),
        "expected two-ref-params error, got: {}",
        err
    );
}

#[test]
fn ref_return_mut_from_shared_param_is_rejected() {
    // `&T -> &mut U` would forge mutability — rejected.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn whoops(p: &Point) -> &mut u32 { &mut p.x }",
    );
    assert!(
        err.contains("cannot return `&mut` from a `&` parameter"),
        "expected mut-from-shared error, got: {}",
        err
    );
}

#[test]
fn mut_method_through_shared_ref_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         impl Point { fn set(&mut self, v: u32) -> u32 { self.x = v; self.x } }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let r: &Point = &pt; \
             r.set(99) \
         }",
    );
    assert!(
        err.contains("&mut self") && err.contains("shared"),
        "expected mut-method-through-shared error, got: {}",
        err
    );
}

#[test]
fn mut_method_on_immutable_owned_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         impl Point { fn set(&mut self, v: u32) -> u32 { self.x = v; self.x } }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             pt.set(99) \
         }",
    );
    assert!(
        err.contains("immutable receiver"),
        "expected immutable-receiver error, got: {}",
        err
    );
}

#[test]
fn assign_through_shared_ref_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn f(p: &Point) -> u32 { p.x = 7; p.x }",
    );
    assert!(
        err.contains("shared reference") || err.contains("not mutable"),
        "expected shared-ref assignment rejection, got: {}",
        err
    );
}

#[test]
fn undeclared_lifetime_is_rejected() {
    // `'a` not declared in the fn's `<'a, ...>` params.
    let err = compile_source(
        "fn bad(x: &'a u32) -> &'a u32 { x }",
    );
    assert!(
        err.contains("undeclared lifetime"),
        "expected undeclared-lifetime error, got: {}",
        err
    );
}

#[test]
fn lifetime_param_after_type_param_is_rejected() {
    // Lifetimes must come before type params (Rust convention).
    let err = compile_source(
        "fn bad<T, 'a>(x: &'a T) -> &'a T { x }",
    );
    assert!(
        err.contains("lifetime parameters must come before"),
        "expected lifetime-after-type rejection, got: {}",
        err
    );
}

#[test]
fn wrong_struct_lifetime_arg_count_is_rejected() {
    // `Holder<'a>` declared, used with two lifetime args.
    let err = compile_source(
        "struct Holder<'a> { r: &'a u32 }\nfn bad<'a, 'b>(h: Holder<'a, 'b>) -> u32 { 0 }",
    );
    assert!(
        err.contains("lifetime arguments"),
        "expected wrong-lifetime-arg-count error, got: {}",
        err
    );
}
