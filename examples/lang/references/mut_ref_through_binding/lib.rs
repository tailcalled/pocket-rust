struct Point { x: u32, y: u32 }

fn modify(p: &mut Point) -> u32 {
    p.x = 99;
    p.y
}

fn answer() -> u32 {
    let mut p = Point { x: 1, y: 2 };
    let r = &mut p;
    let _v = modify(r);
    p.x
}
