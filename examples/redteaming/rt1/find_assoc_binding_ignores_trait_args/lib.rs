// Two `impl Mix<X> for Foo` rows with different `Output` bindings.
// The trait declares `fn mix(self, other: Rhs) -> Self::Output;`,
// where `Self::Output` parses as an AssocProj that the
// impl-validation pass concretizes via `find_assoc_binding`. Because
// that lookup ignores trait_args, both impls' bindings (`u32` and
// `i64`) are returned, the projection stays unresolved, and impl
// registration of the second row fails with a misleading "wrong
// return type: trait declares `<Foo as ?>::Output`, impl has `i64`".
//
// Expected: 42 (the call should dispatch via deferred trait-arg
// inference once the let annotation pins it to u32).

trait Mix<Rhs> {
    type Output;
    fn mix(self, other: Rhs) -> Self::Output;
}

struct Foo {}

impl Mix<u32> for Foo {
    type Output = u32;
    fn mix(self, other: u32) -> u32 { other }
}

impl Mix<i64> for Foo {
    type Output = i64;
    fn mix(self, other: i64) -> i64 { other }
}

fn answer() -> u32 {
    let x: u32 = Foo {}.mix(42u32);
    x
}
