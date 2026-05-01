// Pass a `&[u32]` as a function argument; receive it on the other
// side; observe its length. The fat ref must travel as 2 i32s in
// the function's wasm ABI.
fn count(s: &[u32]) -> u32 {
    s.len() as u32
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(10);
    v.push(20);
    v.push(12);
    v.push(0);
    count(v.as_slice()) + 38
}
