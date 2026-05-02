// `return` with a value exits the function early with that value.
fn pick(b: bool) -> u32 {
    if b {
        return 42;
    }
    0
}

fn answer() -> u32 {
    pick(true)
}
