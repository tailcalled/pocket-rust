// String literal in a match arm — exercises multi-value match-result
// when the if/match returns a string (each arm is a fat ref).
fn pick<'a>(b: bool) -> &'a str {
    if b { "hello" } else { "" }
}

fn answer() -> u32 {
    (pick(true).len() as u32) + 37
}
