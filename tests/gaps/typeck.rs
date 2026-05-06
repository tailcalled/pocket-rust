// Type-checker gaps: cases pocket-rust rejects today but rustc
// accepts. Mostly inference quirks where the integer-literal /
// generic-method dispatch resolution doesn't propagate context far
// enough.

use super::*;

// `let mut x = 0;` infers x as some integer; `x += 1` should desugar
// to `AddAssign::add_assign(&mut x, 1)` and dispatch fine. It does
// at the statement level. But when the compound-assign is nested in
// a block expression that's the first element of a tuple expression,
// pocket-rust's typeck reports:
//   "cannot call `&mut self` method `add_assign` on owned `integer`
//    (no implicit borrow)"
// The implicit-borrow logic for `&mut self` method receivers misses
// this position. Adding `: u32` on the `let mut x` makes it work,
// suggesting the issue is interleaved with int-literal defaulting:
// the compound-assign receiver-typing runs before the literal is
// pinned.
//
// Fix: the implicit `&mut` autoref for compound-assign should fire
// in any expression position where `x = x op rhs` would be valid.
#[test]
fn compound_assign_on_inferred_int_in_tuple_block_is_accepted() {
    let _ = compile_inline(
        "fn answer() -> u32 { \
             let mut x = 0; \
             let _y = ({ x += 1; x }, 99u32).1; \
             x \
         }",
    );
}

// `x + 1` where x is constrained to a single integer type by the
// rest of the function should infer the `1` literal as the same
// kind. This works in straightforward positions. But when the
// expression appears inside a block-then-tuple-then-field-access
// shape (`({ x = x + 1; x }, &(x+1)).1`) with all the integer
// bindings/literals unannotated, typeck reports:
//   "type mismatch: expected `<?0 as std.ops.Add>::Output`, got integer"
// — the `Add` impl resolution can't see through the surrounding
// shape to bind the receiver type. Annotating any of `let mut x`,
// the literals, or the comparison `< 10` to a concrete kind makes
// it work.
//
// Fix: integer-literal defaulting / Add-impl resolution should
// propagate context through tuple field access + block-tail
// positions.
#[test]
fn add_inference_through_tuple_block_is_accepted() {
    let _ = compile_inline(
        "fn answer() -> u32 { \
             let mut x = 0; \
             while x < 10 { \
                 let w = ({ x = x + 1; x }, &(x + 1)).1; \
                 x = *w; \
             } \
             x \
         }",
    );
}

// Calls where both the body and the args are unconstrained num-lit
// Vars (e.g. `let f = |x| x + 1; f.call((5,)) as u32`) leave the
// AssocProj `<?int as Add>::Output` unresolved at typeck — defaulting
// to i32 happens at end-of-fn finalize, too late for the surrounding
// cast. Fix would propagate the cast's expected type back into the
// closure's return-var, or pin the int-default at the call site.
#[test]
fn closure_call_with_default_int_can_cast_is_accepted() {
    let _ = compile_inline(
        "pub fn answer() -> u32 { let f = |x| x + 1; f.call((5,)) as u32 }",
    );
}
