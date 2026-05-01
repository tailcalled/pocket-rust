trait Base {
    fn base(self) -> u32;
}

trait Derived: Base {
    fn derived(self) -> u32;
}

impl Base for (u32, u32) {
    fn base(self) -> u32 { self.0 }
}

impl Derived for (u32, u32) {
    fn derived(self) -> u32 { self.0 + self.1 }
}

fn run<T: Derived>(t: T) -> u32 {
    t.derived()
}

fn answer() -> u32 {
    let p: (u32, u32) = (20, 22);
    run(p)
}
