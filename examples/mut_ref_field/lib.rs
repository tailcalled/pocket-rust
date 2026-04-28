struct Point { x: u32, y: u32 }
struct Pair { first: Point, second: Point }

fn set_x(p: &mut Point) -> u32 {
    p.x = 77;
    p.x
}

fn answer() -> u32 {
    let mut pair = Pair {
        first: Point { x: 1, y: 2 },
        second: Point { x: 3, y: 4 },
    };
    let _v = set_x(&mut pair.second);
    pair.second.x
}
