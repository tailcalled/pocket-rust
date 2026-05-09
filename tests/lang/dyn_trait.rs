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

// `Box<dyn Trait>` — owned trait object. The Box's body is a fat raw
// pointer (data + vtable); coercion `Box<T> → Box<dyn Trait>`
// runs the same obj-safety + impl checks as the ref case.
//
// Today's typeck propagates the let-anno type into `Box::new`'s `T`
// inference, so writing `let b: Box<dyn Show> = Box::new(Foo { v: 42 })`
// is rejected (the call's arg slot is then expected to be `dyn Show`).
// Workaround: bind the source as `Box<Foo>` first, then coerce. Real
// fix is to defer let-anno pinning of generic-fn type-args when a
// dyn coercion is possible at the let boundary — gap-tested.
#[test]
fn box_dyn_show_call_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             fn answer() -> u32 { \
                let bf: Box<Foo> = Box::new(Foo { v: 42 }); \
                let b: Box<dyn Show> = bf; \
                b.show() \
             }",
        )],
        42u32,
    );
}

// Auto-deref-style method dispatch on Box<dyn Trait> without an
// explicit `&*` — the box is its own receiver, codegen extracts the
// fat pointer from the box's flat scalars.
#[test]
fn box_dyn_two_impls_returns_30() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct A { v: u32 }\n\
             struct B { v: u32 }\n\
             impl Show for A { fn show(&self) -> u32 { self.v + 1 } }\n\
             impl Show for B { fn show(&self) -> u32 { self.v + 2 } }\n\
             fn answer() -> u32 { \
                let ba_inner: Box<A> = Box::new(A { v: 10 }); \
                let ba: Box<dyn Show> = ba_inner; \
                let bb_inner: Box<B> = Box::new(B { v: 17 }); \
                let bb: Box<dyn Show> = bb_inner; \
                ba.show() + bb.show() \
             }",
        )],
        30u32,
    );
}

// `&mut self` method dispatch through `Box<dyn Counter>` — the box
// owns its T, so `&mut self` methods are reachable.
#[test]
fn box_dyn_mut_counter_returns_2() {
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
                let bc: Box<Ctr> = Box::new(Ctr { n: 0 }); \
                let mut b: Box<dyn Counter> = bc; \
                b.bump(); b.bump(); \
                b.read() \
             }",
        )],
        2u32,
    );
}

// Pass `Box<dyn Show>` as a fn arg. Same fat shape as `&dyn Show`.
#[test]
fn box_dyn_passed_as_arg_returns_99() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             fn ping(b: Box<dyn Show>) -> u32 { b.show() }\n\
             fn answer() -> u32 { \
                let bf: Box<Foo> = Box::new(Foo { v: 99 }); \
                ping(bf) \
             }",
        )],
        99u32,
    );
}

// Drop dispatch through the vtable's drop slot. The concrete `Logger`
// type has a `Drop` impl that writes a sentinel byte through a raw
// pointer; dropping the `Box<dyn Show>` runs the concrete drop via
// vtable[0], so the sentinel is observable after the box's scope ends.
#[test]
fn box_dyn_drop_fires_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Logger { p: *mut u32 }\n\
             impl Drop for Logger { fn drop(&mut self) { unsafe { *self.p = 42; } } }\n\
             impl Show for Logger { fn show(&self) -> u32 { 0 } }\n\
             fn answer() -> u32 { \
                let mut sentinel: u32 = 0; \
                { \
                    let bl: Box<Logger> = Box::new(Logger { p: &mut sentinel as *mut u32 }); \
                    let _b: Box<dyn Show> = bl; \
                } \
                sentinel \
             }",
        )],
        42u32,
    );
}

// Non-Drop concrete type: the vtable's drop slot points at a no-op fn
// (synthesized at codegen-init), so dropping a `Box<dyn Show>` for a
// non-Drop type doesn't crash.
#[test]
fn box_dyn_drop_noop_for_non_drop_type_returns_7() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Plain { v: u32 }\n\
             impl Show for Plain { fn show(&self) -> u32 { self.v } }\n\
             fn answer() -> u32 { \
                let bp: Box<Plain> = Box::new(Plain { v: 7 }); \
                let b: Box<dyn Show> = bp; \
                b.show() \
             }",
        )],
        7u32,
    );
}

// Negative: Box<dyn Foo> rejected when Foo isn't object-safe.
#[test]
fn box_dyn_obj_unsafe_is_rejected() {
    let err = compile_source(
        "trait Take { fn take(self) -> u32; }\n\
         struct Foo { v: u32 }\n\
         impl Take for Foo { fn take(self) -> u32 { self.v } }\n\
         fn answer() -> u32 { let b: Box<dyn Take> = Box::new(Foo { v: 1 }); 0 }",
    );
    assert!(
        err.contains("by value") || err.contains("object-safe") || err.contains("dyn"),
        "expected obj-safety error on Box<dyn>, got: {}",
        err
    );
}
