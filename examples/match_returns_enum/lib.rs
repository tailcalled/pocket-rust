enum Choice {
    A,
    B,
}

fn flip(c: Choice) -> Choice {
    match c {
        Choice::A => Choice::B,
        Choice::B => Choice::A,
    }
}

fn answer() -> u32 {
    let c: Choice = flip(Choice::A);
    match c {
        Choice::A => 0,
        Choice::B => 42,
    }
}
