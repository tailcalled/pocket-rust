// `const` defined in a sibling module, imported via `use`, used in
// arithmetic. Verifies (1) `pub const` is reachable across modules,
// (2) the imported name resolves at use-site Var lookup, and
// (3) the value inlines correctly.

mod a;

use crate::a::SHIFT;

pub fn answer() -> u32 {
    // Bind to a local first — pocket-rust's binop desugar dispatches
    // through `Add::add` whose receiver is a place expression, and
    // consts aren't (yet) materializable as places. The let-bind is
    // a value-position use of the const, which works.
    let x: u32 = SHIFT;
    x + x
}
