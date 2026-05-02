// Two impls of `Mix<Rhs>` for the same target — inference must pick
// the right one based on usage context. Strategy (d) deferred dispatch:
// at the call site we don't know which impl, so we type-check against
// the trait method's signature with fresh inference vars for `Rhs`,
// then resolution falls out of unifying the result with the let-typed
// binding's annotated type one statement later.

trait Mixer<Rhs> {
    fn mix(self, other: Rhs) -> Rhs;
}

struct Foo {}

impl Mixer<u32> for Foo {
    fn mix(self, other: u32) -> u32 { other + 30 }
}

impl Mixer<i64> for Foo {
    fn mix(self, other: i64) -> i64 { other + 1000 }
}

fn answer() -> u32 {
    let x = Foo {}.mix(12);
    let y: u32 = x;
    y
}
