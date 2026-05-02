// `&s[..]` — `Index<RangeFull> for str`. The full slice; len matches
// the original string's len.
fn answer() -> u32 {
    let s: &str = "hello";
    let all: &str = &s[..];
    all.len() as u32
}
