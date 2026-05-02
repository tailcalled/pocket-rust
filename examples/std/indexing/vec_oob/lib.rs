// Out-of-bounds Vec index — the bounds check fires `panic!`, which
// invokes the host's panic function (a wasm trap in the test
// harness). This function never returns 0.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(7);
    v[5]
}
