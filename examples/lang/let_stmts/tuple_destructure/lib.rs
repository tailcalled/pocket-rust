fn pair() -> (u32, u32) { (30, 12) }

fn answer() -> u32 {
    let (a, b) = pair();
    a + b
}
