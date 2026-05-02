// `&v[..]` — `Index<RangeFull> for Vec<T>`. Whole-Vec slice, length
// matches Vec's len. 4 elements summed: 10+20+30+40 == 100.
fn answer() -> u32 {
    let v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    let all: &[u32] = &v[..];
    all[0] + all[1] + all[2] + all[3]
}
