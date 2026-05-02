// "¥" is U+00A5, encoded as the 2-byte UTF-8 sequence 0xC2 0xA5.
// Slicing "a¥b" at byte index 2 lands in the middle of the ¥ char,
// which would yield an invalid `&str` — the boundary check panics.
fn answer() -> u32 {
    let s: &str = "a¥b";
    let _bad: &str = &s[0..2];
    0u32
}
