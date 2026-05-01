fn answer() -> u32 {
    let mut sum: u32 = 0;
    let mut i: u32 = 0;
    while i < 10 {
        sum = sum + i;
        i = i + 1;
    }
    sum
}
