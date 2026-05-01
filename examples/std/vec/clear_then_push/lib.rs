// `Vec::clear` resets `len` to 0; subsequent push starts fresh.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.clear();
    v.push(42);
    if v.len() == 1 {
        match v.pop() {
            Option::Some(x) => x,
            Option::None => 0,
        }
    } else {
        0
    }
}
