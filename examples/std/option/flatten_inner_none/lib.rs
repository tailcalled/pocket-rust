// `Some(None).flatten()` → `None`.
fn answer() -> u32 {
    let outer: Option<Option<u32>> = Option::Some(Option::None);
    match outer.flatten() {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
