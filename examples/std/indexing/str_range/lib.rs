// `&s[1..4]` — `Index<Range<usize>> for str`. The slice "ello"[1..4]
// is "llo" with len 3.
fn answer() -> u32 {
    let s: &str = "hello";
    let mid: &str = &s[1..4];
    mid.len() as u32
}
