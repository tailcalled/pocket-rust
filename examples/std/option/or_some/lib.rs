// `Some(v).or(b)` returns `Some(v)` regardless of `b`.
fn answer() -> u32 {
    let a: Option<u32> = Option::Some(42);
    let b: Option<u32> = Option::Some(99);
    match a.or(b) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
