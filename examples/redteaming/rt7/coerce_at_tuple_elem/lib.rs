// dyn coercion through tuple-typed slots.
//
// `let t: (&dyn Show, u32) = (&f, 1);` — typeck's let-stmt path
// resolves the annotation, then `coerce_at` runs on the whole
// value-vs-annotation pair. Inside `coerce_at`, the shape match
// requires the OUTER source to be `Ref<T>` and outer target to be
// `Ref<Dyn>`. A tuple wrapper short-circuits that match: the outer
// types are `Tuple([Ref<Foo>, u32])` and `Tuple([Ref<Dyn>, u32])` →
// falls through to plain `unify`, which fails per-element.
//
// Architectural shape: `coerce_at` recognizes only the topmost
// shape, not deep nestings. Real Rust applies unsizing coercions
// inside compound containers via a structural "coercion fold."
// Fix: recursively descend through tuples / struct fields when the
// shapes parallel, applying `coerce_at` at each leaf where a dyn-
// coercion is possible.

trait Show { fn show(&self) -> u32; }
struct Foo { v: u32 }
impl Show for Foo { fn show(&self) -> u32 { self.v } }

pub fn answer() -> u32 {
    let f = Foo { v: 42 };
    // The `&f` inside the tuple should coerce to `&dyn Show`.
    // Today's pocket-rust rejects per-element with
    // "type mismatch: expected `dyn Show`, got `Foo`".
    let t: (&dyn Show, u32) = (&f, 0u32);
    t.0.show()
}
