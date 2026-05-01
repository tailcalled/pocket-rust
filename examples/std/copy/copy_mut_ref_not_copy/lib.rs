fn answer() -> u32 {
    let mut v: u32 = 42;
    let r1: &mut u32 = &mut v;
    let r2: &mut u32 = r1;
    *r2
}
