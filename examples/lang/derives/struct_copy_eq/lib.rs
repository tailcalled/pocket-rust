// Derive Copy + Eq + PartialEq on a struct. Copy lets the struct
// flow through assignments without moving; Eq is a marker.

#[deriving(Copy, Clone, PartialEq, Eq)]
struct Pair { a: u32, b: u32 }

fn answer() -> u32 {
    let p: Pair = Pair { a: 12u32, b: 30u32 };
    // Copy: `p` survives `q = p`.
    let q: Pair = p;
    if p.eq(&q) { p.a + q.b } else { 0u32 }
}
