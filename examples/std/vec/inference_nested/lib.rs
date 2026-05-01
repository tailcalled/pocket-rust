// `Vec<Vec<u32>>` — Vec of Drop-typed elements. Verifies:
//   - inner Vec<u32> moves into outer's buffer (non-Copy push).
//   - outer.pop() returns Option<Vec<u32>>; the Option's payload is
//     a 12-byte Vec, so the sret slot must be 16 bytes (4 disc + 12).
//   - the popped inner Vec is fully usable: we can call v.pop() on it
//     to recover the original u32 element.
//   - Drop semantics on the outer Vec (+ inner Vec inside Option) at
//     scope end: every Drop-typed binding fires its destructor.
//   - **Inference all the way through**: neither `Vec::new()` site
//     gets a type annotation. `inner`'s T is fixed by `inner.push(seed)`
//     where `seed: u32`; `outer`'s T is fixed by `outer.push(inner)`
//     where `inner: Vec<u32>` (a generic-substituted struct, not a
//     primitive). The compiler must propagate the substituted struct
//     type through method dispatch, not just bare params.

fn answer() -> u32 {
    // No annotation on `inner` — T inferred from push of a typed binding.
    let seed: u32 = 42;
    let mut inner = Vec::new();
    inner.push(seed);

    // No annotation on `outer` either — T inferred from `inner: Vec<u32>`
    // being moved in. This forces the inference machinery to pick up
    // `Vec<u32>` (a generic-substituted struct) as the element type.
    let mut outer = Vec::new();
    outer.push(inner);

    match outer.pop() {
        Option::Some(v) => {
            // v is a Vec<u32>. Pop the original 42 back out.
            let mut v_mut = v;
            match v_mut.pop() {
                Option::Some(x) => x,
                Option::None => 0,
            }
        }
        Option::None => 0,
    }
}
