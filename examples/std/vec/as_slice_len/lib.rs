// `Vec::as_slice` produces a fat ref `&[T]`; `[T]::len` reads its
// length half. End-to-end exercise of the slice ABI: 2-i32 fat ref
// passed by-value through a method call.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(10);
    v.push(20);
    v.push(12);
    let s: &[u32] = v.as_slice();
    s.len() as u32
}
