fn answer() -> u32 {
    let x: u32 = 42;
    match x {
        n if n < 10 => 1,
        n if n < 50 => n,
        _ => 100,
    }
}
