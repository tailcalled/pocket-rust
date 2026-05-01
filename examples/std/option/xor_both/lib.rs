// Both `Some` → `xor` returns `None`.
fn answer() -> u32 {
    let a: Option<u32> = Option::Some(7);
    let b: Option<u32> = Option::Some(42);
    match a.xor(b) {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
