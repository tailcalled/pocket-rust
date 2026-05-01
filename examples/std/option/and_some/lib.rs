// `Some(_).and(b)` returns `b` regardless of `b`'s shape.
fn answer() -> u32 {
    let a: Option<u32> = Option::Some(7);
    let b: Option<u32> = Option::Some(42);
    match a.and(b) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
