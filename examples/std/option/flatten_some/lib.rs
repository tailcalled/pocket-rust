// `Some(Some(x)).flatten()` → `Some(x)`.
fn answer() -> u32 {
    let inner: Option<u32> = Option::Some(42);
    let outer: Option<Option<u32>> = Option::Some(inner);
    match outer.flatten() {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
