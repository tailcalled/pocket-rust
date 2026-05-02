// Lexer must distinguish char literal from lifetime: `'a` is a
// lifetime (no closing quote nearby), `'a'` is a char literal.
fn id<'a>(x: &'a u32) -> &'a u32 { x }

fn answer() -> u32 {
    let v: u32 = 32;
    let r = id(&v);
    *r + ('\n' as u32)
}
