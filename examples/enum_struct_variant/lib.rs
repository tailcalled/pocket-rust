enum Shape {
    Square(u32),
    Rect { w: u32, h: u32 },
}

fn answer() -> u32 {
    let _s: Shape = Shape::Rect { w: 6, h: 7 };
    42
}
