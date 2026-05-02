// `matches!` returns false when the pattern doesn't match. Here
// the scrutinee is `None`, so the `Some(_)` arm doesn't fire and
// the wildcard arm produces false.
fn answer() -> u32 {
    let x: Option<u32> = Option::None;
    if matches!(x, Option::Some(_)) { 0 } else { 42 }
}
