// Char literal has type `char`. `as u32` converts to integer.
// `'*'` = U+002A = 42.
fn answer() -> u32 {
    let c: char = '*';
    c as u32
}
