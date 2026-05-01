fn answer() -> u32 {
    let mut sum: u32 = 0;
    let mut i: u32 = 0;
    while i < 10 {
        i = i + 1;
        if i == 5 {
            continue;
        }
        sum = sum + i;
    }
    sum
}
