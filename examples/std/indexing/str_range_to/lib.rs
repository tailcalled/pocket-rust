// `&s[..3]` — `Index<RangeTo<usize>> for str`. "hello"[..3] is
// "hel" with len 3.
fn answer() -> u32 {
    let s: &str = "hello";
    let head: &str = &s[..3];
    head.len() as u32
}
