// `&v[1..3]` — `Index<Range<usize>> for Vec<T>`. The slice has 2
// elements (indices 1 and 2). Sum is 20 + 30 == 50.
fn answer() -> u32 {
    let v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    let s: &[u32] = &v[1..3];
    s[0] + s[1]
}
