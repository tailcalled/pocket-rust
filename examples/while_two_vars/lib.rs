fn answer() -> u32 {
    let mut i: u32 = 0;
    let mut total: u32 = 0;
    while i < 5 {
        total = total + 3;
        i = i + 1;
    }
    total
}
