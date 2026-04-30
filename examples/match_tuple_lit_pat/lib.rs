fn answer() -> u32 {
    let t: (u32, bool) = (40, true);
    match t {
        (x, true) => x + 2,
        (x, false) => x,
    }
}
