// `vec[i] += rhs` desugars to `vec[i].add_assign(rhs)` — a MethodCall
// whose receiver is the indexed place. Lowering must (a) pick
// `IndexMut::index_mut` (because the autoref on `add_assign`'s `&mut
// self` requires a mutable reference) and (b) preserve the synth
// MethodCall when the codegen extracts the receiver's place. An
// earlier `mono_expr_as_place` swap of the inner MethodCall for
// `Local(0)` silently miscompiled this pattern: the placeholder load
// pushed multiple scalars (e.g. `Local(0)` resolved to a struct
// binding), corrupting the wasm stack.

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(30);
    v[0] += 12;
    v[0]
}
