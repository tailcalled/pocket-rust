// PartialOrd on a struct: lexicographic comparison over fields in
// declaration order.

#[derive(PartialEq, PartialOrd)]
struct Point { x: u32, y: u32 }

fn answer() -> u32 {
    let a: Point = Point { x: 1u32, y: 5u32 };
    let b: Point = Point { x: 1u32, y: 7u32 };
    let c: Point = Point { x: 2u32, y: 0u32 };
    // a < b (tie on x, y wins), a < c (x wins), c > b
    if a.lt(&b) && a.lt(&c) && c.gt(&b) && b.le(&b) && c.ge(&a) {
        42u32
    } else {
        0u32
    }
}
