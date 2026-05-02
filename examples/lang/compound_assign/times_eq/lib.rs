// `*=`, `-=`, `/=`, `%=` exercise the other AddAssign-family
// methods. 21 *= 4 → 84; 84 -= 40 → 44; 44 /= 1 → 44; 44 %= 43 → 1;
// then 1 *= 42 → 42.
fn answer() -> u32 {
    let mut x: u32 = 21;
    x *= 4;
    x -= 40;
    x /= 1;
    x %= 43;
    x *= 42;
    x
}
