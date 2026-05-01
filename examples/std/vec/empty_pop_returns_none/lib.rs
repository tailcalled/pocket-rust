// `Vec::pop` on an empty vec returns `None`.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    match v.pop() {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
