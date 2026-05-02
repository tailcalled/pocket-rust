// Trait declares an associated type; impl binds it; method uses
// `Self::Item` in its return type. Resolution: Self → Foo (impl
// target), Self::Item → impl's binding (u32).

trait HasItem {
    type Item;
    fn get(&self) -> Self::Item;
}

struct Foo {
    x: u32,
}

impl HasItem for Foo {
    type Item = u32;
    fn get(&self) -> Self::Item {
        self.x
    }
}

fn answer() -> u32 {
    let f = Foo { x: 42 };
    f.get()
}
