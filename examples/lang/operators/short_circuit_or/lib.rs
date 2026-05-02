// `||` short-circuits: if the lhs is true, the rhs is never
// evaluated. Verified by `panic!()` on the rhs — if `||` evaluated
// it strictly, this example would trap rather than return 42.
fn answer() -> u32 {
    let a: bool = true;
    if a || { panic!("rhs of `||` should not run when lhs is true") } {
        42
    } else {
        0
    }
}
