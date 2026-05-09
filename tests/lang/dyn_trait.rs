// Trait objects: `&dyn Trait` / `&mut dyn Trait`. A `&T` (or `&mut T`)
// where `T: Trait` (and `Trait` is object-safe) coerces into a fat
// reference (data ptr + vtable ptr). Method calls dispatch through
// the vtable via `call_indirect`.
//
// Phase 2 v1 supports single-bound dyn types only; multi-bound,
// supertrait method dispatch, and `dyn Fn`-family closures are
// follow-ups. `Box<dyn Trait>` lives in Phase 3.

use super::*;

// Basic positive: trait Show with two impls; coerce `&Foo` into
// `&dyn Show` at let-anno time, call `.show()` through the vtable.
#[test]
fn dyn_show_calls_through_vtable_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             fn answer() -> u32 { let f: Foo = Foo { v: 42 }; let s: &dyn Show = &f; s.show() }",
        )],
        42u32,
    );
}

// Two distinct concrete types coerced through the same `&dyn Show`
// slot dispatch to different impls. Verifies the vtable is per-(trait,
// concrete-type) and the runtime picks the right slot.
#[test]
fn dyn_show_two_impls_dispatch_separately_returns_30() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct A { v: u32 }\n\
             struct B { v: u32 }\n\
             impl Show for A { fn show(&self) -> u32 { self.v + 1 } }\n\
             impl Show for B { fn show(&self) -> u32 { self.v + 2 } }\n\
             fn answer() -> u32 { \
                let a: A = A { v: 10 }; let b: B = B { v: 17 }; \
                let sa: &dyn Show = &a; let sb: &dyn Show = &b; \
                sa.show() + sb.show() \
             }",
        )],
        30u32,
    );
}

// Pass `&dyn Trait` as a fn arg. The receiver-position fat ref enters
// the callee, which dispatches through the vtable.
#[test]
fn dyn_show_passed_as_arg_returns_77() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             fn ping(s: &dyn Show) -> u32 { s.show() }\n\
             fn answer() -> u32 { let f: Foo = Foo { v: 77 }; ping(&f) }",
        )],
        77u32,
    );
}

// `&mut dyn Trait` with a `&mut self` method that mutates the
// concrete value's field through the fat ref.
#[test]
fn dyn_mut_counter_returns_3() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Counter { fn bump(&mut self); fn read(&self) -> u32; }\n\
             struct Ctr { n: u32 }\n\
             impl Counter for Ctr { \
                fn bump(&mut self) { self.n = self.n + 1; } \
                fn read(&self) -> u32 { self.n } \
             }\n\
             fn answer() -> u32 { \
                let mut c: Ctr = Ctr { n: 0 }; \
                let m: &mut dyn Counter = &mut c; \
                m.bump(); m.bump(); m.bump(); \
                c.read() \
             }",
        )],
        3u32,
    );
}

// Negative: a trait with a by-value `self` receiver isn't object-safe.
#[test]
fn dyn_by_value_self_method_is_rejected() {
    let err = compile_source(
        "trait Take { fn take(self) -> u32; }\n\
         struct Foo { v: u32 }\n\
         impl Take for Foo { fn take(self) -> u32 { self.v } }\n\
         fn answer() -> u32 { let f: Foo = Foo { v: 1 }; let t: &dyn Take = &f; 0 }",
    );
    assert!(
        err.contains("by value") || err.contains("object-safe") || err.contains("dyn"),
        "expected obj-safety error, got: {}",
        err
    );
}

// Negative: a trait with a method-level type parameter isn't object-safe.
#[test]
fn dyn_generic_method_is_rejected() {
    let err = compile_source(
        "trait Map { fn map<U>(&self) -> U; }\n\
         struct Foo {}\n\
         fn answer() -> u32 { let f: Foo = Foo {}; let m: &dyn Map = &f; 0 }",
    );
    assert!(
        err.contains("type parameters") || err.contains("object-safe") || err.contains("dyn"),
        "expected generic-method obj-safety error, got: {}",
        err
    );
}

// Negative: a trait with `Self` in a non-receiver position isn't
// object-safe.
#[test]
fn dyn_self_in_arg_is_rejected() {
    let err = compile_source(
        "trait Eq { fn eq(&self, other: Self) -> bool; }\n\
         struct Foo { v: u32 }\n\
         fn answer() -> u32 { let f: Foo = Foo { v: 1 }; let e: &dyn Eq = &f; 0 }",
    );
    assert!(
        err.contains("Self") || err.contains("object-safe") || err.contains("dyn"),
        "expected Self-outside-receiver error, got: {}",
        err
    );
}

// Negative: `&T` for `T` that doesn't implement the trait can't coerce.
#[test]
fn dyn_no_impl_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(&self) -> u32; }\n\
         struct Foo {}\n\
         fn answer() -> u32 { let f: Foo = Foo {}; let s: &dyn Show = &f; 0 }",
    );
    assert!(
        err.contains("does not implement") || err.contains("no impl"),
        "expected missing-impl error, got: {}",
        err
    );
}

// Negative: calling `&mut self` method on a `&dyn Trait` (not `&mut`)
// is a type error.
#[test]
fn dyn_mut_method_on_immut_dyn_is_rejected() {
    let err = compile_source(
        "trait Counter { fn bump(&mut self); }\n\
         struct Ctr { n: u32 }\n\
         impl Counter for Ctr { fn bump(&mut self) { self.n = self.n + 1; } }\n\
         fn answer() -> u32 { let c: Ctr = Ctr { n: 0 }; let s: &dyn Counter = &c; s.bump(); 0 }",
    );
    assert!(
        err.contains("&mut") || err.contains("mutable"),
        "expected immut-dyn error, got: {}",
        err
    );
}
