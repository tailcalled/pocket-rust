fn answer() -> u32 {
    let pair: (u32, u32) = (10, 30);
    let (mut a, mut b) = pair;
    let r: &mut u32 = &mut a;
    *r = 99;
    let r2: &u32 = &b;
    a + *r2
}
