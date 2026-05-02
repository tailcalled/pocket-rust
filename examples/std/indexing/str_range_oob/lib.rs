// Slicing past the string's end traps with the bounds-check panic.
fn answer() -> u32 {
    let s: &str = "hi";
    let _bad: &str = &s[0..10];
    0u32
}
