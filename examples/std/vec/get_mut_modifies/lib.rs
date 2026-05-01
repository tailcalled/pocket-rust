// `Vec::get_mut(idx)` returns `Some(&mut T)` that we can write through.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    match v.get_mut(1) {
        Option::Some(r) => { *r = 42; }
        Option::None => {}
    }
    match v.get(1) {
        Option::Some(r) => *r,
        Option::None => 0,
    }
}
