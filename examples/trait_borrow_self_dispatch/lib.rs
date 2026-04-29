trait Get { fn get(&self) -> u32; }

struct Foo { x: u32 }

impl Get for Foo {
    fn get(&self) -> u32 { self.x }
}

fn use_get<T: Get>(t: T) -> u32 { t.get() }

fn answer() -> u32 {
    let f: Foo = Foo { x: 42 };
    use_get(f)
}
