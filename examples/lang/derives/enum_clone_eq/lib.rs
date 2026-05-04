// Derive Clone + PartialEq on an enum with mixed variant payloads.

#[derive(Clone, PartialEq)]
enum Shape {
    Empty,
    Pair(u32, u32),
    Named { x: u32 },
}

fn answer() -> u32 {
    let a: Shape = Shape::Pair(20u32, 22u32);
    let b: Shape = a.clone();
    if a.eq(&b) {
        match a {
            Shape::Pair(x, y) => x + y,
            Shape::Empty => 0u32,
            Shape::Named { x } => x,
        }
    } else {
        0u32
    }
}
