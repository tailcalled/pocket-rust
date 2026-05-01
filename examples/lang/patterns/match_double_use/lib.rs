fn answer() -> u32 {
    let p: (u32, u32) = (40, 2);
    match p {
        (a, b) => a + b + a,
    }
}
