// Diagnostic: just push and read len, no get.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(42);
    if v.is_empty() { 0 } else { 42 }
}
