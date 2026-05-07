// `vec![value; count]` — desugars at parse time to a block that
// allocates an empty Vec and pushes `value.clone()` `count` times.
// `T: Clone` is required (Copy types satisfy it via the std blanket).

fn answer() -> u32 {
    let v: Vec<u32> = vec![7u32; 6];
    let mut sum: u32 = 0;
    let mut i: usize = 0;
    while i < v.len() {
        sum += *v.get(i).unwrap_or(&0u32);
        i += 1;
    }
    sum
}
