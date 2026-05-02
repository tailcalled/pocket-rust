// `&s[..]` on a `&[T]` — `Index<RangeFull> for [T]`. Whole-slice
// reborrow; sum of all 4 elements = 100.
fn answer() -> u32 {
    let v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    let s: &[u32] = v.as_slice();
    let all: &[u32] = &s[..];
    all[0] + all[1] + all[2] + all[3]
}
