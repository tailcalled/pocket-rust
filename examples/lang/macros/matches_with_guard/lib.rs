// `matches!(scrut, pattern if guard)` — the optional `if guard`
// after the pattern desugars to a match-arm guard. The pattern
// binding `n` is in scope inside the guard.
fn answer() -> u32 {
    let x: Option<u32> = Option::Some(42);
    if matches!(x, Option::Some(n) if n == 42) { 42 } else { 0 }
}
