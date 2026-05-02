// Unicode escape: '\u{2A}' is U+002A = 42 (the same codepoint as
// '*'). Verifies the `\u{HH..}` lex path.
fn answer() -> u32 {
    '\u{2A}' as u32
}
