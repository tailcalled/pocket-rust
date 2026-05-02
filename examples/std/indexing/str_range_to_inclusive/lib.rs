// `&s[..=2]` — `Index<RangeToInclusive<usize>> for str`. "hello"[..=2]
// is "hel" with len 3 (bytes 0, 1, 2 inclusive).
fn answer() -> u32 {
    let s: &str = "hello";
    let head: &str = &s[..=2];
    head.len() as u32
}
