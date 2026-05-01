// Take `&s` of a slice binding `s: &[u32]`. The escape analysis
// marks `s` as addressed → spilled to a shadow-stack slot of 8
// bytes (the fat-ref size). Reading `s.len()` afterward must go
// through the spilled storage, not the original wasm locals.
fn deref_count(p: &&[u32]) -> u32 {
    (*p).len() as u32
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    let s: &[u32] = v.as_slice();
    let n: u32 = deref_count(&s);  // forces spill of `s`
    let m: u32 = s.len() as u32;   // read after spill — must agree
    (n + m) * 5 + 2
}
