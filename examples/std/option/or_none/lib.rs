// `None.or(b)` returns `b`.
fn answer() -> u32 {
    let a: Option<u32> = Option::None;
    let b: Option<u32> = Option::Some(42);
    match a.or(b) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
