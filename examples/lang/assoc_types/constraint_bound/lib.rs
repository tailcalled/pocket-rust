// `T: HasItem<Item = u32>` constraint pins the assoc type. The
// generic function can then return a u32 from `t.get()`.

trait HasItem {
    type Item;
    fn get(&self) -> Self::Item;
}

struct Foo { x: u32 }

impl HasItem for Foo {
    type Item = u32;
    fn get(&self) -> Self::Item { self.x }
}

fn use_it<T: HasItem<Item = u32>>(t: &T) -> u32 {
    t.get()
}

fn answer() -> u32 {
    let f = Foo { x: 42 };
    use_it(&f)
}
