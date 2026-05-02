// `&s[1..=2]` on a `&[T]` — `Index<RangeInclusive<usize>> for [T]`.
// Sub-slice of length 2 (indices 1 and 2 inclusive); 20 + 30 = 50.
fn answer() -> u32 {
    let v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    let s: &[u32] = v.as_slice();
    let mid: &[u32] = &s[1..=2];
    mid[0] + mid[1]
}
