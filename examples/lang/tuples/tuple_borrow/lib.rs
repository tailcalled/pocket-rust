fn first_of(t: &(u32, u32)) -> u32 {
    t.0
}

fn answer() -> u32 {
    let pair: (u32, u32) = (40, 2);
    first_of(&pair) + pair.1
}
