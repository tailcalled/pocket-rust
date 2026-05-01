struct Point { x: usize, y: usize }
struct Pair { first: usize, second: usize }

fn x_of(p: &Point) -> usize { p.x }
fn y_of(p: &Point) -> usize { p.y }

fn shared_borrows(p: Point) -> Pair {
    Pair {
        first: x_of(&p),
        second: y_of(&p),
    }
}

fn borrow_then_move(p: Point) -> Pair {
    Pair {
        first: x_of(&p),
        second: p.y,
    }
}

fn answer() -> usize {
    borrow_then_move(Point { x: 30, y: 40 }).second
}
