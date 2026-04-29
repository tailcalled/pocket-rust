fn longer<'a>(x: &'a u32, y: &'a u32) -> &'a u32 {
    x
}

fn answer() -> u32 {
    let a: u32 = 42;
    let b: u32 = 99;
    let r: &u32 = longer(&a, &b);
    *r
}
