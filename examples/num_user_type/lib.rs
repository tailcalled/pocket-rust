struct Wrap { v: u32 }

impl Num for Wrap {
    fn from_i64(x: i64) -> Wrap { Wrap { v: x as u32 } }
}

fn answer() -> u32 {
    let w: Wrap = 42;
    w.v
}
