// Labeled `while let`: `'outer: while let …`. The label propagates
// onto the synthesized while-loop, so an inner `break 'outer` from
// a nested loop exits the labeled while-let.

fn answer() -> u32 {
    let mut outer_state: Option<u32> = Option::Some(0u32);
    let mut total: u32 = 0u32;
    'outer: while let Option::Some(n) = outer_state {
        let mut inner: u32 = 0u32;
        while inner < 3u32 {
            if n == 2u32 && inner == 1u32 {
                break 'outer;
            }
            total = total + 1u32;
            inner = inner + 1u32;
        }
        outer_state = Option::Some(n + 1u32);
    }
    // n=0: inner runs 3 times → total=3
    // n=1: inner runs 3 times → total=6
    // n=2: inner=0 → total=7, inner=1 → break 'outer
    total + 35u32
}
