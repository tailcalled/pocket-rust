struct Point {
    x: u32,
    y: u32,
}

fn answer() -> u32 {
    let p: Point = Point { x: 40, y: 2 };
    match p {
        Point { x, y } => x + y,
    }
}
