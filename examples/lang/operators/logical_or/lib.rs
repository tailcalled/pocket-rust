// `||` desugars to `if lhs { true } else { rhs }`.
fn answer() -> u32 {
    let a: bool = false;
    let b: bool = false;
    let c: bool = true;
    if a || b || c { 42 } else { 0 }
}
