// `panic!` lives on the cold path — when the condition is false,
// the function returns 42 normally without invoking the host panic.
fn check(x: u32) -> u32 {
    if x == 0 {
        panic!("zero")
    } else {
        x
    }
}

fn answer() -> u32 {
    check(42)
}
