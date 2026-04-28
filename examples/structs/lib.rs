struct Point { x: usize, y: usize }
struct Rect { top_left: Point, bottom_right: Point }
struct Diagram { primary: Rect, secondary: Rect }

fn point(x: usize, y: usize) -> Point {
    Point { x: x, y: y }
}

fn flip(p: Point) -> Point {
    Point { x: p.y, y: p.x }
}

fn rect(tl: Point, br: Point) -> Rect {
    Rect { top_left: tl, bottom_right: br }
}

fn diagram() -> Diagram {
    Diagram {
        primary: rect(point(1, 2), point(3, 4)),
        secondary: rect(flip(point(10, 20)), flip(point(30, 40))),
    }
}

fn outer_corners(d: Diagram) -> Rect {
    Rect {
        top_left: d.primary.top_left,
        bottom_right: d.secondary.bottom_right,
    }
}

fn quadrant(d: Diagram) -> Point {
    flip(d.secondary.bottom_right)
}

fn answer() -> usize {
    rect(
        quadrant(diagram()),
        outer_corners(diagram()).bottom_right,
    ).top_left.y
}
