enum Shape {
    Square { side: u32 },
    Rect { w: u32, h: u32 },
}

fn answer() -> u32 {
    let s: Shape = Shape::Rect { w: 6, h: 7 };
    match s {
        Shape::Square { side } => side,
        Shape::Rect { w, h } => w * h,
    }
}
