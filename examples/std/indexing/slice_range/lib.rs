// `&s[1..3]` on a `&[T]` — `Index<Range<usize>> for [T]`.
// Sub-slice of length 2; elements 20, 30; sum = 50.
fn answer() -> u32 {
    let v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    let s: &[u32] = v.as_slice();
    let mid: &[u32] = &s[1..3];
    mid[0] + mid[1]
}
