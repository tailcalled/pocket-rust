// `break` inside a `while let` body exits the synthesized loop —
// the same as in a regular while-loop.

fn answer() -> u32 {
    let mut current: Option<u32> = Option::Some(0u32);
    let mut sum: u32 = 0u32;
    while let Option::Some(n) = current {
        if n >= 4u32 {
            break;
        }
        sum = sum + n;
        current = Option::Some(n + 1u32);
    }
    // 0 + 1 + 2 + 3 = 6, then break before adding 4
    sum + 36u32
}
