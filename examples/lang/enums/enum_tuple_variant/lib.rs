enum Choice {
    A(u32, u32),
    B,
}

fn answer() -> u32 {
    let _c: Choice = Choice::A(40, 2);
    42
}
