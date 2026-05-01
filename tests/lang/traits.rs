// Trait declarations, impls, dispatch, autoref disambiguation,
// supertraits, and trait-impl validation.

use super::*;

// Trait surface: declarations, `impl Trait for Type`, blanket
// `impl<T> Trait for &T`, trait bounds on generics. Validates basic
// structure.
#[test]
fn trait_decl_and_impl_compiles() {
    expect_answer("lang/traits/trait_decl_and_impl", 42i32);
}

// T2: concrete trait method dispatch via `impl Show for Foo` +
// `f.show()`.
#[test]
fn trait_concrete_dispatch_returns_42() {
    expect_answer("lang/traits/trait_concrete_dispatch", 42i32);
}

// T2: recursive impl resolution. `Wrap<Wrap<u32>>: Show` requires
// matching `impl<T: Show> Show for Wrap<T>` twice and ultimately
// `impl Show for u32`. Codegen produces three distinct mono'd `show`
// functions.
#[test]
fn trait_recursive_wrap_returns_42() {
    expect_answer("lang/traits/trait_recursive_wrap", 42i32);
}

// T2: symbolic dispatch in a generic body via the type-param's bound.
// `t.show()` inside `fn use_show<T: Show>(t: T)` resolves through `T:
// Show` and re-dispatches to the concrete impl at mono time.
#[test]
fn trait_bound_dispatch_returns_42() {
    expect_answer("lang/traits/trait_bound_dispatch", 42i32);
}

// T2.5: trait dispatch through `&self` autoref'ing an owned generic
// receiver. `t.get()` inside `fn use_get<T: Get>(t: T)` where Get
// takes `&self` must autoref `t` before the trait call.
#[test]
fn trait_borrow_self_dispatch_returns_42() {
    expect_answer("lang/traits/trait_borrow_self_dispatch", 42i32);
}

// T2.5b: trait methods with their own type-params. `Pick::pick<U>`
// declares a method-level type-param. The impl on `First` carries a
// matching `<U>` (validated α-equivalently). At a symbolic call
// `t.pick::<u32>(11, 22)` through a `T: Pick` bound, codegen
// monomorphizes `First::pick<u32>`.
#[test]
fn trait_method_generic_returns_11() {
    expect_answer("lang/traits/trait_method_generic", 11i32);
}

// T2.6: concrete trait dispatch on a primitive recv. `x.show()` for
// `x: u32` finds `impl Show for u32` even though the impl_target
// isn't a struct path.
#[test]
fn trait_impl_on_u32_returns_42() {
    expect_answer("lang/traits/trait_impl_on_u32", 42i32);
}

// T2.6: blanket impl `impl<T> Show for &T` dispatches when the recv
// is a `&Foo` and Foo doesn't otherwise implement Show.
#[test]
fn trait_blanket_on_ref_returns_42() {
    expect_answer("lang/traits/trait_blanket_on_ref", 42i32);
}

// T2.6.5: when type-pattern matching yields multiple candidates, drop
// those whose `derive_recv_adjust` would error.
#[test]
fn dispatch_adjust_filter_returns_99() {
    expect_answer("lang/traits/dispatch_adjust_filter", 99i32);
}

// T2.7: with `r: &u32; r.show()`, the `&T` blanket impl matches at
// peel-level 0 while `impl Show for u32` matches via autoref at level
// 1. The direct match wins.
#[test]
fn autoref_disambig_through_ref_returns_2() {
    expect_export("lang/traits/autoref_disambig", "through_ref", 2i32);
}

// Sanity check the inverse: with `x: u32` (owned), `impl Show for
// u32` matches directly while the blanket matches via pattern-side
// autoref.
#[test]
fn autoref_disambig_through_owned_returns_1() {
    expect_export("lang/traits/autoref_disambig", "through_owned", 1i32);
}

// Pattern-side autoref reaching a blanket impl: only `impl<T> Tag
// for &T` exists, recv is owned `x: u32`. Pattern `&T` matches via
// autoref (T=u32). derive_recv_adjust says BorrowImm — `&x` is
// passed to the impl method.
#[test]
fn autoref_only_returns_7() {
    expect_answer("lang/traits/autoref_only", 7i32);
}

// Partial-concrete impl: `impl<T> Pair<usize, T>` matches `Pair<u32,
// T>` for any T's substitution. Method dispatches via try_match on
// impl_target.
#[test]
fn impl_partial_concrete_returns_42() {
    expect_answer("lang/traits/impl_partial_concrete", 42i32);
}

// Repeat-param impl: `impl<T> Pair<T, T>` only matches when both
// type args coincide. Matching binds T once and unifies the second
// occurrence.
#[test]
fn impl_repeat_param_returns_42() {
    expect_answer("lang/traits/impl_repeat_param", 42i32);
}

// Fully-concrete impl: zero impl type-params, target is concrete.
#[test]
fn impl_fully_concrete_returns_42() {
    expect_answer("lang/traits/impl_fully_concrete", 42i32);
}

#[test]
fn impl_trait_for_bool_returns_42() {
    // Exercises `try_match_rtype` Bool arm: supertrait obligation
    // for `impl Derived for bool` requires matching `Bool` against
    // the `impl Base for bool` row's Bool target. derived(true)=32 +
    // base(true)=10 = 42.
    expect_answer("lang/traits/impl_trait_for_bool", 42u32);
}

#[test]
fn impl_trait_for_tuple_returns_42() {
    // `try_match_rtype` Tuple arm: supertrait obligation matches
    // `(u32, u32)` against the Base impl's tuple target. 20 + 22 = 42.
    expect_answer("lang/traits/impl_trait_for_tuple", 42u32);
}

#[test]
fn impl_trait_for_enum_returns_42() {
    // `try_match_rtype` Enum arm: supertrait obligation matches
    // `Choice` against the Base impl's enum target.
    expect_answer("lang/traits/impl_trait_for_enum", 42u32);
}

#[test]
fn supertrait_methods_through_bound_returns_22() {
    // `<T: Dog>` (Dog: Animal) reaches Animal::legs through the
    // supertrait. legs=4 + bark=7 = 11; *2 = 22.
    expect_answer("lang/traits/supertrait_methods_through_bound", 22u32);
}

#[test]
fn ambiguous_impl_method_dispatch_is_rejected() {
    // Two impls' targets both match `Pair<u32, u32>` and both define
    // `get`.
    let err = compile_source(
        "struct Pair<T, U> { first: T, second: U }\n\
         impl<T> Pair<u32, T> { fn get(self) -> u32 { self.first } }\n\
         impl<U> Pair<U, u32> { fn get(self) -> u32 { self.second } }\n\
         fn f() -> u32 { let p: Pair<u32, u32> = Pair { first: 1, second: 2 }; p.get() }",
    );
    assert!(
        err.contains("ambiguous method"),
        "expected ambiguous-method error, got: {}",
        err
    );
}

#[test]
fn duplicate_impl_block_is_rejected() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Foo { fn get(self) -> u32 { self.x } }\n\
         impl Foo { fn get(self) -> u32 { self.x } }\n\
         fn f() -> u32 { let f: Foo = Foo { x: 7 }; f.get() }",
    );
    assert!(
        err.contains("ambiguous method") || err.contains("duplicate"),
        "expected duplicate/ambiguous-impl error, got: {}",
        err
    );
}

#[test]
fn missing_trait_method_in_impl_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("missing trait method"),
        "expected missing-method error, got: {}",
        err
    );
}

#[test]
fn extra_method_in_trait_impl_is_rejected() {
    let err = compile_source(
        "trait Show {}\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn extra(self) -> u32 { self.x } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("not a member of trait"),
        "expected extra-method error, got: {}",
        err
    );
}

#[test]
fn duplicate_trait_impl_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn show(self) -> u32 { self.x } }\n\
         impl Show for Foo { fn show(self) -> u32 { self.x } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("duplicate impl"),
        "expected duplicate-impl error, got: {}",
        err
    );
}

#[test]
fn trait_impl_concretizes_method_param_is_rejected() {
    // When the trait method has its own type-param `<U>`, the impl
    // must declare a matching one and use it polymorphically.
    let err = compile_source(
        "trait Foo { fn bar<U>(self, u: U) -> U; }\n\
         struct X {}\n\
         impl Foo for X { fn bar(self, u: u32) -> u32 { u } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("type parameters"),
        "expected method type-param arity error, got: {}",
        err
    );
}

#[test]
fn trait_impl_method_return_type_mismatch_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn show(self) -> u64 { 0 } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("wrong return type"),
        "expected return-type-mismatch error, got: {}",
        err
    );
}

#[test]
fn trait_impl_method_param_type_mismatch_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self, n: u32) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn show(self, n: u64) -> u32 { 0 } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("wrong parameter type"),
        "expected param-type-mismatch error, got: {}",
        err
    );
}

#[test]
fn trait_method_without_bound_is_rejected() {
    // Calling `t.show()` inside `fn f<T>(t: T)` (no `T: Show` bound)
    // should be rejected.
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         fn f<T>(t: T) -> u32 { t.show() }",
    );
    assert!(
        err.contains("no method `show`") || err.contains("no trait bound"),
        "expected missing-bound error, got: {}",
        err
    );
}

#[test]
fn unknown_trait_in_impl_is_rejected() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Bogus for Foo {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("unknown trait"),
        "expected unknown-trait error, got: {}",
        err
    );
}

#[test]
fn impl_without_supertrait_impl_is_rejected() {
    // `trait Sub: Super` requires `impl Super for T` to exist
    // whenever `impl Sub for T` is registered.
    let err = compile_source(
        "trait Super { fn s(&self) -> u32; }\n\
         trait Sub: Super { fn x(&self) -> u32; }\n\
         struct Foo { n: u32 }\n\
         impl Sub for Foo { fn x(&self) -> u32 { self.n } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("trait bound") && err.contains("Super"),
        "expected supertrait obligation error, got: {}",
        err
    );
}

#[test]
fn self_outside_impl_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: Self = 0; x }",
    );
    assert!(
        err.contains("`Self` is only valid inside an `impl` block"),
        "expected Self-outside-impl error, got: {}",
        err
    );
}
