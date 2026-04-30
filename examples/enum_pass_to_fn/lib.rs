enum Choice {
    A,
    B,
}

fn use_choice(_c: Choice) -> u32 {
    42
}

fn answer() -> u32 {
    use_choice(Choice::A)
}
