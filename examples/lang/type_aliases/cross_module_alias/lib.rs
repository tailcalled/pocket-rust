// Cross-module alias resolution: module `a` defines `pub type X = u32`,
// module `b` brings it into scope as `Y` via `use crate::a::X as Y;`,
// then uses `Y` in a function signature. Verifies that:
//   - `pub type` is exported from a child module.
//   - `use ... as` re-import sees the alias and binds the local name
//     `Y` to the alias's full path `["a", "X"]`.
//   - `Y` resolves through the alias to `u32` at typeck.

mod a;
mod b;

fn answer() -> u32 {
    b::compute(20u32)
}
