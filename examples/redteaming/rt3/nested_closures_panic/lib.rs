// `closure_lower::rewrite_expr` rewrites a closure expression by
// replacing it with a struct-lit and synthesizing impl items — but
// the synthesized impl method's body comes from
// `clone_expr_fresh_ids(&closure.body, ...)` where the body's
// `Closure` arm is `unreachable!("inner closures must be rewritten
// before clone_expr_fresh_ids")`. The traversal in `rewrite_expr`
// processes children FIRST, but the Closure case at the bottom of
// the function pulls `closure.body` out and clones it WITHOUT first
// rewriting nested closures inside that body. So a closure whose
// body contains a closure expression hits the `unreachable!` and
// the compiler panics rather than producing a diagnostic or correct
// code.
//
// Architectural shape: the lowering is a single pre-order pass that
// rewrites at the visited node, but nested `ExprKind::Closure` nodes
// inside `closure.body` are unreachable from the outer rewrite once
// the body is consumed via `std::mem::replace`. Two fixable shapes:
// (1) recurse into `closure.body` BEFORE the outer replacement, so
// inner closures get their own rewrite + synth-impl emitted first;
// (2) make `clone_expr_fresh_ids` handle `Closure(_)` instead of
// panicking — recursing into nested closures, allocating IDs, and
// returning a cloned Closure that a subsequent walk picks up. (1)
// is simpler and matches how the rest of `closure_lower` already
// thinks about the AST.
//
// At runtime: pocket-rust panics inside the compile() call (no
// wasm produced). The test asserts the compile fails, but the
// failure is a panic, not a clean Error.
//
// Expected post-fix: program compiles and `answer()` returns 8.

pub fn answer() -> u32 {
    let outer: u32 = 7u32;
    let make_inner = |_unit: ()| -> u32 {
        let g = |_unit2: ()| outer + 1u32;
        g.call(((),))
    };
    make_inner.call(((),))
}
