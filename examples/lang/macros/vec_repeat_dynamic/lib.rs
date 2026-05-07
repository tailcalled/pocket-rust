// `vec![value; count]` with a dynamic count expression. Verifies the
// count is evaluated once (it's bound to a synth local before the
// loop starts) so an expression with side effects fires exactly once.

fn answer() -> u32 {
    let n: usize = 4;
    let v: Vec<bool> = vec![true; n];
    let mut count: u32 = 0;
    let mut i: usize = 0;
    while i < v.len() {
        if *v.get(i).unwrap_or(&false) { count += 1u32; }
        i += 1;
    }
    count * 11u32 - 2u32
}
