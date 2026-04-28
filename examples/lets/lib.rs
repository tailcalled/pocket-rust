struct Point { x: usize, y: usize }

fn x_of(p: &Point) -> usize { p.x }

fn answer() -> usize {
    let p = Point { x: 5, y: 10 };
    let r = &p;
    x_of(r)
}
