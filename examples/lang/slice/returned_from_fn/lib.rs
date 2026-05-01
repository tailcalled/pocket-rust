// Return a `&[T]` from a function. The caller receives 2 i32s.
// Multi-value wasm result.
fn forward<'a>(s: &'a [u32]) -> &'a [u32] {
    s
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(7);
    v.push(8);
    let s = forward(v.as_slice());
    s.len() as u32 + 40
}
