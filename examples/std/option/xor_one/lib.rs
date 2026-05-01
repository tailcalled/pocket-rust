// `xor` returns `Some` when exactly one is `Some`.
fn answer() -> u32 {
    let a: Option<u32> = Option::Some(42);
    let b: Option<u32> = Option::None;
    match a.xor(b) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
