// `let x: u32;` followed by an `if` that assigns on every path. At
// the join point both sides are Init, so the trailing read passes
// borrowck.
fn pick(b: bool) -> u32 {
    let x: u32;
    if b {
        x = 7u32;
    } else {
        x = 35u32;
    }
    x
}

fn answer() -> u32 {
    pick(true) + pick(false)
}
