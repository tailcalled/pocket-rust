fn answer() -> u32 {
    let x: u32 = 42;
    match x {
        n @ 0..=10 => n,
        n @ 11..=100 => n,
        _ => 0,
    }
}
