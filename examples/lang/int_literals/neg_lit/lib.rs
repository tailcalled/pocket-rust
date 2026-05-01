// `-INT_LIT` parses as a negative literal. The literal's type infers
// from context (here `isize` via the let annotation), so the literal
// pins to isize and the `-1` round-trips back as -1.
fn answer() -> u32 {
    let n: isize = -1;
    let p: isize = 43;
    (p + n) as u32
}
