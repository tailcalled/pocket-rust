// Pattern-match on `Option<T>` directly via the prelude-imported variants.
fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42);
    match o {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
