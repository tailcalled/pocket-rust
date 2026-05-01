trait Base {
    fn base(self) -> u32;
}

trait Derived: Base {
    fn derived(self) -> u32;
}

impl Base for bool {
    fn base(self) -> u32 { if self { 10 } else { 0 } }
}

impl Derived for bool {
    fn derived(self) -> u32 { if self { 32 } else { 0 } }
}

fn run<T: Derived>(t: T) -> u32 {
    let a: u32 = t.derived();
    a
}

fn answer() -> u32 {
    let b: bool = true;
    run(b) + b.base()
}
