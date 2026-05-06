// `closure_lower::clone_expr_fresh_ids` rewrites `Var(captured_name)`
// → `self.<name>` lexically: it walks the body and rewrites EVERY Var
// matching a captured name, with no scope tracking. When the body
// shadows a captured name with an inner `let`, the rewrite still
// fires for the inner reference and silently swaps in the captured
// value where the user's source said "use the inner local".
//
// Architectural shape: capture-rewrite is purely lexical against the
// captures' name set. The fix needs scope tracking — track which
// names are introduced by inner let-statements / inner closure
// params and skip the rewrite for shadowed Vars. Equivalent to
// running a small name-resolution pass over the cloned body.
//
// At runtime: this program prints 2000 (= 1000 + 1000 — both `outer`
// references rewritten to the captured value) instead of 1005
// (= 1000 from the captured `outer` + 5 from the inner `let outer`).
//
// Expected post-fix: returns 1005.

pub fn answer() -> u32 {
    let outer: u32 = 1000u32;
    let f = |_unit: ()| -> u32 {
        let a: u32 = outer;
        let outer: u32 = 5u32;
        let b: u32 = outer;
        a + b
    };
    f.call(((),))
}
