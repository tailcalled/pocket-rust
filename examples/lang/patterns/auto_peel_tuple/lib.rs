// Match ergonomics: a tuple pattern against `&(u32, u32)` auto-peels.

fn pick(t: &(u32, u32)) -> u32 {
    match t {
        (a, b) => *a + *b,
    }
}

fn answer() -> u32 {
    let t: (u32, u32) = (10u32, 32u32);
    pick(&t)
}
