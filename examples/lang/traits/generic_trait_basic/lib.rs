// Trait with positional type-param: `Mix<Rhs>` lets the impl pick a
// distinct RHS type. Validates the parsing, storage, and basic
// dispatch path for generic-trait params. Only one impl exists so
// dispatch is unambiguous; multi-impl trait-args dispatch needs
// arg-driven candidate filtering (see follow-up).

trait Mix<Rhs> {
    fn mix(self, other: Rhs) -> u32;
}

struct Foo { x: u32 }

impl Mix<u32> for Foo {
    fn mix(self, other: u32) -> u32 { self.x + other }
}

fn answer() -> u32 {
    let f = Foo { x: 20 };
    f.mix(22)
}
