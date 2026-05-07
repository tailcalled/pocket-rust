// `while let Some(x) = …` — repeatedly evaluate the scrutinee and
// bind on each match. Loop exits when the pattern fails to match
// (here when `decrement` returns None on the zero step).

fn decrement(n: u32) -> Option<u32> {
    if n == 0u32 {
        Option::None
    } else {
        Option::Some(n - 1u32)
    }
}

fn answer() -> u32 {
    let mut current: Option<u32> = Option::Some(5u32);
    let mut count: u32 = 0u32;
    while let Option::Some(n) = current {
        count = count + n;
        current = decrement(n);
    }
    // 5 + 4 + 3 + 2 + 1 = 15
    count + 27u32
}
