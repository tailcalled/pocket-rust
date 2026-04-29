trait Show { fn show(self) -> u32; }

struct Foo { x: u32 }

impl Show for Foo {
    fn show(self) -> u32 { self.x }
}

fn answer() -> u32 {
    let f: Foo = Foo { x: 42 };
    f.show()
}
