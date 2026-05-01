struct Point { x: u32, y: u32 }

fn bump(p: &mut Point) -> u32 {
    p.x = 50;
    p.y = 7;
    p.x
}

fn answer() -> u32 {
    let mut p = Point { x: 1, y: 2 };
    let _v = bump(&mut p);
    p.x
}
