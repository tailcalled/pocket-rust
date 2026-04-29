struct Point { x: u32, y: u32 }

fn x_of(p: &Point) -> u32 {
    p.x
}

fn answer() -> u32 {
    let pt = Point { x: 7, y: 35 };
    let _r: &Point = &pt;
    let _v = x_of(&pt);
    pt.x
}
