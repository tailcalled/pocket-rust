// `[T]::get(idx)` returns `None` past the end.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    let s: &[u32] = v.as_slice();
    match s.get(7) {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
