trait Tag {
    fn tag(&self) -> u32;
}

impl<T> Tag for &T {
    fn tag(&self) -> u32 { 7 }
}

fn answer() -> u32 {
    let x: u32 = 42;
    x.tag()
}
