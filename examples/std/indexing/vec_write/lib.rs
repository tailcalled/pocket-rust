// `vec[idx] = val` dispatches to `Vec::index_mut` (IndexMut trait)
// and stores through the returned `&mut T`.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(0);
    v.push(0);
    v[0] = 10;
    v[1] = 32;
    v[0] + v[1]
}
