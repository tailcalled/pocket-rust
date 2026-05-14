// Borrowck doesn't trace borrows through `&T → &dyn Trait`
// coercions.
//
// Typeck records `DynCoercion` per expr id; borrowck's
// `borrows.rs::rtype_contains_ref` claims `Dyn { .. }` IS ref-bearing
// (conservatively), so a `&dyn Trait` value *should* propagate
// borrows. But the actual propagation reads the RECORDED type at the
// outer expr (which is `&dyn Trait`), not the inner source type
// (`&T`). The `RefDynCoerce` mono node carries `src_concrete_ty` for
// codegen's vtable, but borrowck doesn't see it: it reads the
// surface AST and the recorded `expr_types[id]`, sees `&dyn Show`,
// emits region constraints against `&dyn Show`'s outer lifetime —
// and forgets that the underlying place is `&Foo` rooted at a local
// `f`.
//
// Architectural shape: dyn coercions are a typeck-side invention;
// borrowck rebuilds the value-flow CFG from the AST + expr_types and
// never reads `dyn_coercions`. Region inference can't enforce the
// inner local's lifetime against the outer `&dyn`'s lifetime — the
// connection is missing.
//
// Real Rust rejects: `f` doesn't outlive `'static`, so returning a
// `&'static dyn Show` derived from `&f` is unsound.

trait Show { fn show(&self) -> u32; }
struct Foo { v: u32 }
impl Show for Foo { fn show(&self) -> u32 { self.v } }

// Returns a `&'static dyn Show` derived from a stack local. Real
// Rust rejects: the borrow can't escape past `f`'s scope. Today's
// pocket-rust accepts (the coercion erases the inner ref's region
// trace) and the wasm trap depends on what the caller does next.
fn dangling() -> &'static dyn Show {
    let f = Foo { v: 42 };
    let s: &dyn Show = &f;
    s
}

pub fn answer() -> u32 {
    dangling().show()
}
