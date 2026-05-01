// Verify each recognized escape produces the expected byte. We
// concatenate via separate len reads — `len` on `"\n\r\t\\\"\0"`
// should be exactly 6 bytes (one per escape).
fn answer() -> u32 {
    let s: &str = "\n\r\t\\\"\0";
    (s.len() as u32) * 7  // 6 * 7 = 42
}
