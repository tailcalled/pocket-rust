// Trait associated types: declaration in traits, binding in impls,
// `Self::Name` / `T::Name` projection, and `Trait<Name = X>` bound
// constraints.

use super::*;

// Trait declares `type Item;`, impl provides `type Item = u32;`,
// method body returns `Self::Item` (resolved via the impl's binding).
#[test]
fn basic_assoc_returns_42() {
    expect_answer("lang/assoc_types/basic_assoc", 42i32);
}

// Two impls of the same trait with different assoc-type bindings.
// Each call dispatches to its impl's binding (Item=u32 vs Item=u64).
#[test]
fn multi_impl_diff_bindings_returns_42() {
    expect_answer("lang/assoc_types/multi_impl_diff_bindings", 42i64);
}

// `<T: HasItem<Item = u32>>` constraint pins the assoc type so the
// generic body can use it concretely.
#[test]
fn constraint_bound_returns_42() {
    expect_answer("lang/assoc_types/constraint_bound", 42i32);
}

// Negative: impl missing a binding for an assoc the trait declared.
#[test]
fn impl_missing_assoc_binding_is_rejected() {
    let err = compile_source(
        "trait HasItem { type Item; fn get(&self) -> Self::Item; }\n\
         struct Foo { x: u32 }\n\
         impl HasItem for Foo { fn get(&self) -> u32 { self.x } }\n\
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("missing associated type binding") && err.contains("Item"),
        "expected missing-assoc-binding error, got: {}",
        err
    );
}

// Negative: impl declares an assoc the trait doesn't have.
#[test]
fn impl_extra_assoc_binding_is_rejected() {
    let err = compile_source(
        "trait HasItem { fn get(&self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl HasItem for Foo { type Item = u32; fn get(&self) -> u32 { self.x } }\n\
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("not a member of trait") && err.contains("Item"),
        "expected extra-assoc-binding error, got: {}",
        err
    );
}

// Negative: inherent impls can't carry assoc bindings.
#[test]
fn inherent_impl_assoc_binding_is_rejected() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Foo { type Item = u32; fn x(&self) -> u32 { self.x } }\n\
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("only allowed in trait impls"),
        "expected inherent-impl-assoc-binding error, got: {}",
        err
    );
}

// Negative: caller passes a type whose impl's assoc binding doesn't
// satisfy the function's `Trait<Item = u32>` constraint. Inferred
// `T = Bar`; Bar's impl has `Item = u64`, but the bound demands
// `Item = u32`. Caught statically at the call site.
#[test]
fn constraint_bound_mismatch_at_callsite_is_rejected() {
    let err = compile_source(
        "trait HasItem { type Item; fn get(&self) -> Self::Item; }\n\
         struct Bar { x: u64 }\n\
         impl HasItem for Bar { type Item = u64; fn get(&self) -> u64 { self.x } }\n\
         fn use_it<T: HasItem<Item = u32>>(t: &T) -> u32 { t.get() }\n\
         fn answer() -> u32 { let b = Bar { x: 1 }; use_it(&b) }",
    );
    assert!(
        err.contains("associated type") && err.contains("Item")
            && err.contains("u32") && err.contains("u64"),
        "expected assoc-type-mismatch error, got: {}",
        err
    );
}

// Negative: caller passes a type that doesn't implement the trait at
// all (so no impl can satisfy the assoc constraint). Surface a
// "trait bound not satisfied" diagnostic.
#[test]
fn constraint_bound_no_impl_is_rejected() {
    let err = compile_source(
        "trait HasItem { type Item; fn get(&self) -> Self::Item; }\n\
         struct Baz { x: u32 }\n\
         fn use_it<T: HasItem<Item = u32>>(t: &T) -> u32 { t.get() }\n\
         fn answer() -> u32 { let b = Baz { x: 1 }; use_it(&b) }",
    );
    assert!(
        !err.is_empty() && err.contains("Item"),
        "expected no-impl rejection, got: {}",
        err
    );
}

// Negative: turbofish-pinned T doesn't satisfy the bound's assoc
// constraint. Same kind of error, surfaced via explicit type-arg.
#[test]
fn constraint_bound_turbofish_mismatch_is_rejected() {
    let err = compile_source(
        "trait HasItem { type Item; fn get(&self) -> Self::Item; }\n\
         struct Bar { x: u64 }\n\
         impl HasItem for Bar { type Item = u64; fn get(&self) -> u64 { self.x } }\n\
         fn use_it<T: HasItem<Item = u32>>(t: &T) -> u32 { t.get() }\n\
         fn answer() -> u32 { let b = Bar { x: 1 }; use_it::<Bar>(&b) }",
    );
    assert!(
        err.contains("associated type") && err.contains("Item"),
        "expected turbofish-mismatch error, got: {}",
        err
    );
}

// Negative: duplicate `type Name = ...` inside one impl body.
#[test]
fn impl_duplicate_assoc_binding_is_rejected() {
    let err = compile_source(
        "trait HasItem { type Item; fn get(&self) -> Self::Item; }\n\
         struct Foo { x: u32 }\n\
         impl HasItem for Foo { \
             type Item = u32; \
             type Item = u64; \
             fn get(&self) -> u32 { self.x } \
         }\n\
         fn answer() -> u32 { 0 }",
    );
    assert!(
        err.contains("duplicate associated type binding"),
        "expected duplicate-binding error, got: {}",
        err
    );
}
