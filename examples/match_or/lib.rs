fn answer() -> u32 {
    let x: u32 = 5;
    match x {
        1 | 2 | 3 => 10,
        4 | 5 | 6 => 42,
        _ => 0,
    }
}
