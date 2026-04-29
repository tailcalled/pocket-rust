fn passthru<'a>(x: &'a u32) -> &'a u32 {
    x
}

fn pick_first<'a>(x: &'a u32, y: &u32) -> &'a u32 {
    x
}

fn answer() -> u32 {
    let v: u32 = 42;
    let r: &u32 = passthru(&v);
    let other: u32 = 99;
    let r2: &u32 = pick_first(r, &other);
    *r2
}
