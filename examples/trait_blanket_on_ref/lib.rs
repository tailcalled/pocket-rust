trait Show { fn show(self) -> u32; }

struct Foo { x: u32 }

impl<T> Show for &T { fn show(self) -> u32 { 42 } }

fn answer() -> u32 {
    let f: Foo = Foo { x: 0 };
    let r: &Foo = &f;
    r.show()
}
