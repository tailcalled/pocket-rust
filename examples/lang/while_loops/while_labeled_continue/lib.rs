fn answer() -> u32 {
    let mut count: u32 = 0;
    let mut i: u32 = 0;
    'outer: while i < 3 {
        i = i + 1;
        let mut j: u32 = 0;
        while j < 5 {
            j = j + 1;
            if j == 2 {
                continue 'outer;
            }
            count = count + 1;
        }
        count = count + 100;
    }
    count
}
