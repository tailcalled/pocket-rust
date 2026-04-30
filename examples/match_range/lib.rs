fn answer() -> u32 {
    let x: u32 = 25;
    match x {
        0..=9 => 0,
        10..=99 => 42,
        _ => 100,
    }
}
