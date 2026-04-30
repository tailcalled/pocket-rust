enum Pair {
    Some(u32, u32),
    None,
}

fn answer() -> u32 {
    let p: Pair = Pair::Some(40, 2);
    match p {
        Pair::Some(a, b) => a + b,
        Pair::None => 0,
    }
}
