fn answer() -> u32 {
    let mut total: u32 = 0;
    let mut i: u32 = 0;
    while i < 4 {
        let mut j: u32 = 0;
        while j < 3 {
            total = total + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    total
}
