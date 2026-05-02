// `break` typed as `!` lets it sit as one arm of an `if` whose other
// arm yields a real value — the if's type is the real value's type.
// Without `!`, this wouldn't compile (break would type as `()`,
// mismatching the else arm's u32).

fn answer() -> u32 {
    let mut sum: u32 = 0;
    let mut i: u32 = 0;
    while i < 100 {
        let step: u32 = if i == 7 { break } else { 1 };
        sum = sum + step;
        i = i + 1;
    }
    sum + 35
}
