// `&s[2..]` — `Index<RangeFrom<usize>> for str`. "hello"[2..] is
// "llo" with len 3.
fn answer() -> u32 {
    let s: &str = "hello";
    let tail: &str = &s[2..];
    tail.len() as u32
}
