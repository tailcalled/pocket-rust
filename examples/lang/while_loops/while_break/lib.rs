fn answer() -> u32 {
    let mut i: u32 = 0;
    while i < 100 {
        if i == 42 {
            break;
        }
        i = i + 1;
    }
    i
}
