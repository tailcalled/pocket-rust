// `Vec::get(idx)` returns `None` past the current length.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    match v.get(7) {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
