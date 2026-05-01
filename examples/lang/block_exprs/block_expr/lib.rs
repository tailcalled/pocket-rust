struct Point { x: usize, y: usize }

fn x_of(p: &Point) -> usize { p.x }

fn answer() -> usize {
    let result = {
        let p = Point { x: 11, y: 22 };
        x_of(&p)
    };
    result
}
