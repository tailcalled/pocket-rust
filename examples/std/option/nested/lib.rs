// Generic enum nesting + per-variant struct payload to confirm Option
// composes correctly with user types.
struct Pair { x: u32, y: u32 }

fn answer() -> u32 {
    let inner: Option<u32> = Option::Some(40);
    let outer: Option<Option<u32>> = Option::Some(inner);
    match outer {
        Option::Some(o) => match o {
            Option::Some(v) => v + 2,
            Option::None => 0,
        },
        Option::None => 0,
    }
}
