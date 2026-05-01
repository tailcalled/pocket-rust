struct Point { x: usize, y: usize }

fn x_of(p: &Point) -> usize { p.x }

fn answer() -> usize {
    let pt1 = Point { x: 42, y: 0 };
    let pt2 = { let pt3 = &pt1; pt3 };
    x_of(pt2)
}
