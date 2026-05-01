fn answer() -> u32 {
    let mut total: u32 = 0;
    let mut i: u32 = 0;
    'outer: while i < 10 {
        let mut j: u32 = 0;
        while j < 10 {
            if i == 4 {
                if j == 2 {
                    break 'outer;
                }
            }
            total = total + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    total
}
