// `if cond { slice_a } else { slice_b }` — the if's result type is
// `&[u32]`, which flattens to 2 i32s. The wasm if needs a multi-
// value BlockType (TypeIdx) to carry both halves out.
fn pick<'a>(cond: bool, a: &'a [u32], b: &'a [u32]) -> &'a [u32] {
    if cond { a } else { b }
}

fn answer() -> u32 {
    let mut v1: Vec<u32> = Vec::new();
    v1.push(1);
    v1.push(2);
    let mut v2: Vec<u32> = Vec::new();
    v2.push(10);
    v2.push(20);
    v2.push(30);
    let s = pick(false, v1.as_slice(), v2.as_slice());
    (s.len() as u32) + 39
}
