// `&[T]` as a tuple element. Tuple layout: tightly packed in
// declaration order. (u32, &[u32]) totals 12 bytes (4 + 8).
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(7);
    v.push(8);
    v.push(9);
    v.push(10);
    let t: (u32, &[u32]) = (38, v.as_slice());
    t.0 + (t.1.len() as u32)
}
