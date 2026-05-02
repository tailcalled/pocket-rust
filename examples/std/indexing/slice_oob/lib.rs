// Out-of-bounds slice read — bounds check in `[T]::index` panics
// with "slice index out of bounds".
fn read(s: &[u32], i: usize) -> u32 {
    s[i]
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(7);
    read(v.as_slice(), 4)
}
