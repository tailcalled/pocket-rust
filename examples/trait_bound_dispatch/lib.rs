trait Show { fn show(self) -> u32; }

struct Foo { x: u32 }

impl Show for Foo {
    fn show(self) -> u32 { self.x }
}

fn use_show<T: Show>(t: T) -> u32 { t.show() }

fn answer() -> u32 {
    let f: Foo = Foo { x: 42 };
    use_show(f)
}
