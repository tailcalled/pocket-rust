fn answer() -> u32 {
    let x: u32 = 42;
    let r: &u32 = &x;
    match r {
        &n => n,
    }
}
