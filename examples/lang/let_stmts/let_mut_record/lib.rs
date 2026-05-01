struct Point { x: u32, y: u32 }

fn answer() -> u32 {
    let mut p = Point { x: 5, y: 10 };
    p.x = 99;
    p.y = 7;
    p.x
}
