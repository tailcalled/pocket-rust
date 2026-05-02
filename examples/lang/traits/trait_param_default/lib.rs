// `trait Mix<Rhs = Self>`: the impl writes `impl Mix for Foo` (no
// trait-args), and Rhs defaults to Self = Foo. Validates that
// trait-arg defaulting fills in `Self` correctly at the impl site
// and that downstream method dispatch sees the right Rhs (Foo, not
// some sentinel).

trait Mix<Rhs = Self> {
    fn mix(self, other: Rhs) -> u32;
}

struct Foo { x: u32 }

impl Mix for Foo {
    fn mix(self, other: Foo) -> u32 { self.x + other.x }
}

fn answer() -> u32 {
    let a = Foo { x: 20 };
    let b = Foo { x: 22 };
    a.mix(b)
}
