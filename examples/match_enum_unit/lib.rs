enum Choice {
    A,
    B,
}

fn answer() -> u32 {
    let c: Choice = Choice::B;
    match c {
        Choice::A => 0,
        Choice::B => 42,
    }
}
