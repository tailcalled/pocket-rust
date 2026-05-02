// `matches!(scrut, pattern)` desugars at parse time to
// `match scrut { pattern => true, _ => false }`. Exercises the
// pattern arm: `Option::Some(_)` matches.
fn answer() -> u32 {
    let x: Option<u32> = Option::Some(7);
    if matches!(x, Option::Some(_)) { 42 } else { 0 }
}
