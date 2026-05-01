// `"hello"` is `&'static str` — a 5-byte fat ref into the data section.
fn answer() -> u32 {
    let s: &str = "hello";
    (s.len() as u32) + 37
}
