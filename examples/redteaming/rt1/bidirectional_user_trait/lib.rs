// Bidirectional inference / lazy-projection scenario on a
// **non-arithmetic** user trait. `84` is a num-lit Var. `.halve()`
// is a method declared on a user trait `Halver` with multiple Int
// impls. Rust resolves this by deferring the trait selection until
// the let binding's `: u32` annotation pins the result type, then
// searches Halver's impls for one with `Out = u32` — finds the
// `impl Halver for u32` row uniquely, dispatches it, returns 42.
//
// pocket-rust today: the num-lit dispatch path consults a
// hardcoded `numeric_lit_op_trait_paths()` list and never sees user
// traits, so the call surfaces "no method `halve` on `integer`".
// Dropping the global collapse heuristic and replacing it with
// proper lazy projection (dynamic trait discovery + back-prop on
// AssocProj-vs-concrete) is what enables this case.
//
// Expected: 42.

trait Halver {
    type Out;
    fn halve(self) -> Self::Out;
}

impl Halver for u32 { type Out = u32; fn halve(self) -> u32 { self / 2 } }
impl Halver for u64 { type Out = u64; fn halve(self) -> u64 { self / 2 } }

fn answer() -> u32 {
    let x: u32 = 84.halve();
    x
}
