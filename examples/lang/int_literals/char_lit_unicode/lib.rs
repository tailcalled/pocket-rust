// Multi-byte UTF-8 char literal: '¥' = U+00A5 = 165 (encoded as
// 0xC2 0xA5 in source bytes, decoded by the lexer to codepoint 165).
// 165 - 123 = 42.
fn answer() -> u32 {
    '¥' as u32 - 123
}
