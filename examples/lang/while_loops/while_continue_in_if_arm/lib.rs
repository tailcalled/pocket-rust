// `continue` typed as `!` likewise sits as one arm of an `if`. Skip
// odd values, sum 0..=9 evens = 0+2+4+6+8 = 20; +22 = 42.

fn answer() -> u32 {
    let mut sum: u32 = 0;
    let mut i: u32 = 0;
    while i < 10 {
        let v: u32 = if i % 2 == 1 {
            i = i + 1;
            continue
        } else {
            i
        };
        sum = sum + v;
        i = i + 1;
    }
    sum + 22
}
