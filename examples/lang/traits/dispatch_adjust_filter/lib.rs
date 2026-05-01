trait Show { fn show(self) -> u32; }

struct Foo { x: u32 }

impl Show for Foo { fn show(self) -> u32 { self.x } }
impl<T: Show> Show for &T { fn show(self) -> u32 { 99 } }

fn answer() -> u32 {
    let f: Foo = Foo { x: 0 };
    let r: &Foo = &f;
    r.show()
}
