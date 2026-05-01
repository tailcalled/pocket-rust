enum Choice {
    A,
    B(u32),
}

trait Base {
    fn base(self) -> u32;
}

trait Derived: Base {
    fn derived(self) -> u32;
}

impl Base for Choice {
    fn base(self) -> u32 {
        match self {
            Choice::A => 0,
            Choice::B(n) => n,
        }
    }
}

impl Derived for Choice {
    fn derived(self) -> u32 {
        match self {
            Choice::A => 1,
            Choice::B(n) => n,
        }
    }
}

fn run<T: Derived>(t: T) -> u32 {
    t.derived()
}

fn answer() -> u32 {
    run(Choice::B(42))
}
