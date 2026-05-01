// `[T]::get_mut(idx)` returns `Some(&mut T)` we can write through.
// Routes via `Vec::as_mut_slice` to construct the `&mut [T]`.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    {
        let s: &mut [u32] = v.as_mut_slice();
        match s.get_mut(1) {
            Option::Some(r) => { *r = 42; }
            Option::None => {}
        }
    }
    match v.get(1) {
        Option::Some(r) => *r,
        Option::None => 0,
    }
}
