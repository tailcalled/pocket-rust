// `&s[1..=3]` — `Index<RangeInclusive<usize>> for str`. "hello"[1..=3]
// is "ell" with len 3 (bytes 1, 2, 3 inclusive).
fn answer() -> u32 {
    let s: &str = "hello";
    let mid: &str = &s[1..=3];
    mid.len() as u32
}
