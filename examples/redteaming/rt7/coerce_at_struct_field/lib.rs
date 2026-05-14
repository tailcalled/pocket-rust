// dyn coercion at struct-literal field initializers.
//
// `coerce_at` (typeck/mod.rs) is the only path that detects the
// `&T → &dyn Trait` (and `Box<T> → Box<dyn Trait>`) shape and records
// a `DynCoercion`. It's invoked at four sites: let-stmt RHS, fn-call
// args, fn return-tail, and fn return-expr. Struct-literal field
// initializers (`check_struct_lit` in typeck) coerce the field
// value via plain `ctx.subst.coerce`, which doesn't see the dyn
// pattern.
//
// Architectural shape: the dyn-coercion machinery treats coercion
// sites as a finite enumerated list; every value-flow boundary
// where a `&T → &dyn` could naturally occur must be added to that
// list. Struct literals are the obvious omission. Fix: route field
// initializers (and probably tuple constructors, enum-variant
// payloads, struct-update syntax) through `coerce_at` with the
// field-init node id.

trait Show { fn show(&self) -> u32; }
struct Foo { v: u32 }
impl Show for Foo { fn show(&self) -> u32 { self.v } }

struct Holder<'a> { f: &'a dyn Show + 'a }

pub fn answer() -> u32 {
    let f = Foo { v: 42 };
    // Field `f` is `&'a dyn Show`; we're initializing it with `&f`
    // (a `&Foo`). Real Rust accepts via unsizing coercion; today's
    // pocket-rust rejects with "type mismatch: expected `dyn Show`,
    // got `Foo`".
    let h: Holder = Holder { f: &f };
    h.f.show()
}
