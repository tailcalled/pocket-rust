// Empty string literal — len = 0.
fn answer() -> u32 {
    let s: &str = "";
    if s.is_empty() { 42 } else { 0 }
}
