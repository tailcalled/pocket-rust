enum Choice {
    A,
    B,
}

fn pick() -> Choice {
    Choice::A
}

fn answer() -> u32 {
    let _c: Choice = pick();
    42
}
