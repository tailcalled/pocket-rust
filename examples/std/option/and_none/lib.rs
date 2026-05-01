// `None.and(b)` returns `None`. Test by checking the answer fell into
// the None branch (so we return 42 from the None arm).
fn answer() -> u32 {
    let a: Option<u32> = Option::None;
    let b: Option<u32> = Option::Some(7);
    match a.and(b) {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
