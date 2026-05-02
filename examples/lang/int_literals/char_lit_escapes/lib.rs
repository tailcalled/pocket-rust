// Common escape sequences in char literals.
// '\n' = 10, '\t' = 9, '\\' = 92, '\0' = 0.
// 10 + 9 + 92 + 0 - 69 = 42.
fn answer() -> u32 {
    '\n' as u32 + '\t' as u32 + '\\' as u32 + '\0' as u32 - 69
}
