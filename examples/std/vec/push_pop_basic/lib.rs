// `Vec::push` followed by `Vec::pop` returns the pushed value.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(42);
    match v.pop() {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
