// Out-of-bounds Vec write — `Vec::index_mut` bounds-checks too.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(0);
    v[3] = 42;
    0
}
