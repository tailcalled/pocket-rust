struct Point { x: u32, y: u32 }
struct Pair { first: Point, second: Point }

fn answer() -> u32 {
    let mut p = Pair {
        first: Point { x: 1, y: 2 },
        second: Point { x: 3, y: 4 },
    };
    p.first.x = 99;
    p.second.y = 7;
    p.first.x
}
