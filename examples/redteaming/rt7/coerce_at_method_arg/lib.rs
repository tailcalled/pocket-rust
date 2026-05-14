// dyn coercion at method-call arguments.
//
// `check_method_call` (and `check_dyn_method_call`) type-check method
// args via plain `ctx.subst.coerce`, not `coerce_at`. Fn-call args
// were updated in Phase 2 to use `coerce_at`; method-call args were
// missed.
//
// Architectural shape: the four `coerce_at` sites in typeck have
// drifted — fn-call args use the dyn-aware helper, method-call args
// don't. Any value-flow boundary into a `&dyn Trait` slot via a
// method's parameter list slips through. Fix: route method-call arg
// coercion through `coerce_at` with the arg expression's node id.

trait Show { fn show(&self) -> u32; }
struct Foo { v: u32 }
impl Show for Foo { fn show(&self) -> u32 { self.v } }

struct Holder;
impl Holder {
    // Method takes `&dyn Show`. Calling it with `&Foo` should
    // unsize-coerce.
    fn take(&self, s: &dyn Show) -> u32 { s.show() }
}

pub fn answer() -> u32 {
    let h = Holder {};
    let f = Foo { v: 42 };
    // Real Rust accepts: `&f` (a `&Foo`) coerces into the `&dyn Show`
    // method parameter slot. Today's pocket-rust rejects with
    // "type mismatch: expected `dyn Show`, got `Foo`".
    h.take(&f)
}
