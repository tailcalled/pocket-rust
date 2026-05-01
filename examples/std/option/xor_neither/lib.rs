// Both `None` → `xor` returns `None`.
fn answer() -> u32 {
    let a: Option<u32> = Option::None;
    let b: Option<u32> = Option::None;
    match a.xor(b) {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
