// Smoke: derive Clone and PartialEq on a struct with primitive fields.
// Cloning produces an equal value; comparing two distinct instances
// with the same field values returns true.

#[derive(Clone, PartialEq)]
struct Pair { a: u32, b: u32 }

fn answer() -> u32 {
    let p: Pair = Pair { a: 12u32, b: 30u32 };
    let q: Pair = p.clone();
    if p.eq(&q) { p.a + p.b } else { 0u32 }
}
