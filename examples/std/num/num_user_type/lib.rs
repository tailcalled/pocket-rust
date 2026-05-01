struct Wrap { v: u32 }

impl Num for Wrap {
    fn from_i64(x: i64) -> Wrap { Wrap { v: x as u32 } }
    fn add(self, other: Wrap) -> Wrap { Wrap { v: ¤u32_add(self.v, other.v) } }
    fn sub(self, other: Wrap) -> Wrap { Wrap { v: ¤u32_sub(self.v, other.v) } }
    fn mul(self, other: Wrap) -> Wrap { Wrap { v: ¤u32_mul(self.v, other.v) } }
    fn div(self, other: Wrap) -> Wrap { Wrap { v: ¤u32_div(self.v, other.v) } }
    fn rem(self, other: Wrap) -> Wrap { Wrap { v: ¤u32_rem(self.v, other.v) } }
}

fn answer() -> u32 {
    let w: Wrap = 42;
    w.v
}
