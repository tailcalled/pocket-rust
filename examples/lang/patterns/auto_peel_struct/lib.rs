// Match ergonomics: a struct pattern matched against `&Pair` auto-peels
// the reference, and inner field bindings get the inherited Ref mode.

struct Pair { a: u32, b: u32 }

fn pick(p: &Pair) -> u32 {
    match p {
        Pair { a, b } => *a + *b,
    }
}

fn answer() -> u32 {
    let p: Pair = Pair { a: 12u32, b: 30u32 };
    pick(&p)
}
