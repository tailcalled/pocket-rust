// `vec[i] += N` should work — `vec[i]` resolves to `IndexMut::index_mut`
// when in mutable context, giving `&mut T`, and `add_assign` would
// take `&mut self`. Plain assignment `vec[i] = …;` works (it routes
// through IndexMut codegen). Compound assignment fails for the same
// reason as the deref case: `is_mutable_place` only walks
// Var/FieldAccess/TupleIndex chains, returning false for `Index`.
// Dispatch then can't promote the call to `&mut Self` and errors with
// "no method `add_assign` on `u32`".
//
// Expected: 42.

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(30);
    v[0] += 12;
    v[0]
}
