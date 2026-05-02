// Two impls of the same trait with different associated-type
// bindings. Exercises the impl-row lookup keyed on (trait, target).

trait HasItem {
    type Item;
    fn get(&self) -> Self::Item;
}

struct A { v: u32 }
struct B { v: u64 }

impl HasItem for A {
    type Item = u32;
    fn get(&self) -> Self::Item { self.v }
}

impl HasItem for B {
    type Item = u64;
    fn get(&self) -> Self::Item { self.v }
}

fn answer() -> u64 {
    let a = A { v: 10 };
    let b = B { v: 32 };
    a.get() as u64 + b.get()
}
