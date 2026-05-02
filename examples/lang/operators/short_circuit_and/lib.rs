// `&&` short-circuits: if the lhs is false, the rhs is never
// evaluated. Verified by `panic!()` on the rhs — if `&&` evaluated
// it strictly, this example would trap rather than return 42.
fn answer() -> u32 {
    let a: bool = false;
    if a && { panic!("rhs of `&&` should not run when lhs is false") } {
        0
    } else {
        42
    }
}
