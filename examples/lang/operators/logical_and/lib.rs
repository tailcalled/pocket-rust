// `&&` desugars to `if lhs { rhs } else { false }`.
fn answer() -> u32 {
    let a: bool = true;
    let b: bool = true;
    let c: bool = false;
    if a && b && !c { 42 } else { 0 }
}
