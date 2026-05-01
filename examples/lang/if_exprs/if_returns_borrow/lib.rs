fn pick<'a>(b: bool, x: &'a u32, y: &'a u32) -> &'a u32 {
    if b { x } else { y }
}

fn answer() -> u32 {
    let a: u32 = 42;
    let c: u32 = 99;
    let r: &u32 = pick(true, &a, &c);
    *r
}
