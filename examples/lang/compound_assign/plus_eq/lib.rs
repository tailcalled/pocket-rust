// `+=` desugars to `AddAssign::add_assign(&mut a, b)`. Each call
// reads `a` through `&mut self` and writes back. Loop sums
// 6 + 7 + 8 + 9 + 10 = 40, then `+= 2` brings the total to 42.

fn answer() -> u32 {
    let mut acc: u32 = 0;
    let mut i: u32 = 6;
    while i < 11 {
        acc += i;
        i += 1;
    }
    acc += 2;
    acc
}
