// `[T]::get(idx)` returns `Some(&T)` for an in-bounds index.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(10);
    v.push(20);
    v.push(42);
    v.push(99);
    let s: &[u32] = v.as_slice();
    match s.get(2) {
        Option::Some(r) => *r,
        Option::None => 0,
    }
}
