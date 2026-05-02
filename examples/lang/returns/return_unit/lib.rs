// Bare `return` (no value) in a unit-returning function.
fn helper(skip: bool) {
    if skip {
        return;
    }
}

fn answer() -> u32 {
    helper(true);
    helper(false);
    42
}
