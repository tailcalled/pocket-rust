// `&v[2..]` — `Index<RangeFrom<usize>> for Vec<T>`. Tail of length
// (4-2)=2: elements 30 and 40. Sum is 70.
fn answer() -> u32 {
    let v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    let tail: &[u32] = &v[2..];
    tail[0] + tail[1]
}
