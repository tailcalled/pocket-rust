// Prefix `!x` desugars to `x.not()` via `std::ops::Not`. For `bool`
// this lowers to `¤bool_not`.
fn answer() -> u32 {
    let a: bool = false;
    if !a { 42 } else { 0 }
}
