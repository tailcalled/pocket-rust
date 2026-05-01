fn answer() -> u32 {
    let nested: (u32, (u32, u32)) = (10, (20, 12));
    nested.0 + nested.1.0 + nested.1.1
}
