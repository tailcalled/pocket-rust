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

// `&dyn Trait` from one site passing into a fn taking `&dyn Trait`
// at another site — the source's already-Dyn inner shouldn't retrigger
// the coercion + impl-existence check (which would fail vacuously).
// Tests the "source-already-Dyn → fall through to unify" guard in
// `coerce_at`.
#[test]
fn dyn_passes_through_unchanged_returns_33() {
    expect_answer_sources(
        &[
            ("lib.rs", "mod inner;\nuse crate::inner::Show;\n\
             struct A { v: u32 }\n\
             impl Show for A { fn show(&self) -> u32 { self.v } }\n\
             fn take(s: &dyn Show) -> u32 { s.show() }\n\
             fn answer() -> u32 { \
                let a: A = A { v: 33 }; \
                let s: &dyn Show = &a; \
                take(s) \
             }"),
            ("inner.rs", "pub trait Show { fn show(&self) -> u32; }"),
        ],
        33u32,
    );
}

// Direct `let b: Box<dyn Show> = Box::new(Foo { ... })` — the
// annotation's `T = dyn Show` doesn't pre-pin Box::new's type-arg
// because typeck checks the value expression first (without the
// annotation hint), then coerce_at runs at the boundary.
#[test]
fn box_dyn_direct_coercion_returns_50() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             fn answer() -> u32 { \
                let b: Box<dyn Show> = Box::new(Foo { v: 50 }); \
                b.show() \
             }",
        )],
        50u32,
    );
}

// Generic-impl vtable: `&Wrap<u32>` coerces to `&dyn Show` through
// `impl<T> Show for Wrap<T>`. The impl method is a `GenericTemplate`,
// not a non-generic `FnSymbol`; `intern_vtable` monomorphizes it via
// `mono_table.intern` to get a wasm idx for the slot.
#[test]
fn dyn_generic_impl_vtable_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             struct Wrap<T> { v: T }\n\
             impl<T> Show for Wrap<T> { fn show(&self) -> u32 { 42 } }\n\
             fn answer() -> u32 { \
                let w: Wrap<u32> = Wrap { v: 7 }; \
                let s: &dyn Show = &w; \
                s.show() \
             }",
        )],
        42u32,
    );
}

// Supertrait method dispatch: `&dyn Show` where `trait Show: Tag`
// can also dispatch `.tag()` through the supertrait Tag. The vtable
// includes Show's methods plus Tag's (BFS through supertrait closure,
// skipping unsafe methods).
#[test]
fn dyn_supertrait_method_dispatch_returns_99() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Tag { fn tag(&self) -> u32; }\n\
             trait Show: Tag { fn show(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Tag for Foo { fn tag(&self) -> u32 { self.v + 7 } }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             fn answer() -> u32 { \
                let f: Foo = Foo { v: 92 }; \
                let s: &dyn Show = &f; \
                s.tag() \
             }",
        )],
        99u32,
    );
}

// `&dyn A + B` — multi-bound trait object. Vtable concatenates A's
// method slots after B's (post-drop-header). Dispatch finds which
// bound declares the method and uses its absolute slot offset.
#[test]
fn dyn_multi_bound_dispatch_returns_55() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "trait Show { fn show(&self) -> u32; }\n\
             trait Tag { fn tag(&self) -> u32; }\n\
             struct Foo { v: u32 }\n\
             impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
             impl Tag for Foo { fn tag(&self) -> u32 { self.v + 5 } }\n\
             fn answer() -> u32 { \
                let f: Foo = Foo { v: 25 }; \
                let d: &dyn Show + Tag = &f; \
                d.show() + d.tag() \
             }",
        )],
        55u32,
    );
}

// Negative: ambiguous method on multi-bound dyn.
#[test]
fn dyn_multi_bound_ambiguous_method_is_rejected() {
    let err = compile_source(
        "trait A { fn name(&self) -> u32; }\n\
         trait B { fn name(&self) -> u32; }\n\
         struct Foo {}\n\
         impl A for Foo { fn name(&self) -> u32 { 1 } }\n\
         impl B for Foo { fn name(&self) -> u32 { 2 } }\n\
         fn answer() -> u32 { let f: Foo = Foo {}; let d: &dyn A + B = &f; d.name() }",
    );
    assert!(
        err.contains("ambiguous"),
        "expected ambiguous-method error, got: {}",
        err
    );
}

// Negative: one bound not object-safe.
#[test]
fn dyn_multi_bound_obj_unsafe_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(&self) -> u32; }\n\
         trait Take { fn take(self) -> u32; }\n\
         struct Foo { v: u32 }\n\
         impl Show for Foo { fn show(&self) -> u32 { self.v } }\n\
         impl Take for Foo { fn take(self) -> u32 { self.v } }\n\
         fn answer() -> u32 { let f: Foo = Foo { v: 1 }; let d: &dyn Show + Take = &f; 0 }",
    );
    assert!(
        err.contains("by value") || err.contains("object-safe"),
        "expected obj-safety error on multi-bound, got: {}",
        err
    );
}

// `&dyn Fn(T) -> R` — a trait object for the closure Fn family.
// Closures are object-safe by special handling: the `Fn::call`
// method takes `&self`, and `FnOnce::call_once`'s by-value receiver
// is exempted (supertrait methods aren't part of `dyn Fn`'s vtable).
// The dyn type's `(u32,)` trait-args + `Output = u32` assoc binding
// drive method-signature substitution at dispatch.
#[test]
fn dyn_fn_call_returns_42() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn answer() -> u32 { \
                let cls = |x: u32| -> u32 { x + 1 }; \
                let f: &dyn Fn(u32) -> u32 = &cls; \
                f(41) \
             }",
        )],
        42u32,
    );
}

// `Box<dyn Fn(T) -> R>` — owned trait object for closures.
#[test]
fn box_dyn_fn_call_returns_100() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn answer() -> u32 { \
                let cls = |x: u32| -> u32 { x * 2 }; \
                let f: Box<dyn Fn(u32) -> u32> = Box::new(cls); \
                f(50) \
             }",
        )],
        100u32,
    );
}

// Pass `&dyn Fn` as a fn arg.
#[test]
fn dyn_fn_passed_as_arg_returns_7() {
    expect_answer_sources(
        &[(
            "lib.rs",
            "fn apply(f: &dyn Fn(u32) -> u32, x: u32) -> u32 { f(x) }\n\
             fn answer() -> u32 { \
                let cls = |x: u32| -> u32 { x + 5 }; \
                apply(&cls, 2) \
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
