fn answer() -> u32 {
    let mut t: (u32, u32) = (10, 20);
    t.0 = 22;
    t.0 + t.1
}
