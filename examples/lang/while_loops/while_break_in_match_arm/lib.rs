// `break` typed as `!` works the same in a `match` arm position —
// other arms yield real values; the match's type is u32.

fn answer() -> u32 {
    let mut sum: u32 = 0;
    let mut i: u32 = 0;
    while i < 100 {
        let step: u32 = match i {
            7 => break,
            _ => 1,
        };
        sum = sum + step;
        i = i + 1;
    }
    sum + 35
}
