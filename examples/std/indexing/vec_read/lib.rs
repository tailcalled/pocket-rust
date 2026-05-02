// `vec[idx]` in value position dispatches to `Vec::index` (Index
// trait) and dereferences the returned `&T`.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(10);
    v.push(32);
    v[0] + v[1]
}
