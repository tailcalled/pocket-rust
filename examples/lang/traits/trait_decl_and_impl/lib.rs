trait Show { fn show(self) -> u32; }

trait Marker {}

struct Foo { x: u32 }

impl Show for Foo {
    fn show(self) -> u32 { self.x }
}

impl Marker for Foo {}

impl<T> Marker for &T {}

struct Wrap<T> { inner: T }

impl<T> Show for Wrap<T> {
    fn show(self) -> u32 { 0 }
}

fn opaque<T: Show + Marker>(t: T) -> u32 { 0 }

fn answer() -> u32 {
    let f: Foo = Foo { x: 42 };
    f.x
}
