// Slicing with start > end traps with the reversed-range panic.
fn answer() -> u32 {
    let s: &str = "hello";
    let _bad: &str = &s[3..1];
    0u32
}
