// `std::ops::Drop` — destructor calls at scope-end, Drop on
// function params, conditional drops with flags, partial-move-of-
// Drop rejection, Drop/Copy mutual exclusion.

use super::*;

// Dropping a Logger at the inner block's scope end writes 1 to a
// shared counter via `*self.counter = 1`. The outer fn reads `c`
// after the block, observing the drop side effect.
#[test]
fn drop_logger_returns_1() {
    expect_answer("std/drop/drop_logger", 1i32);
}

// T4.5: drops at block-expression scope end. The inner block has a
// tail (`42`) and a Drop binding (`_l`). Codegen saves the tail
// value to a local, runs `_l`'s drop (writes 7 to c), reloads the
// tail, then the outer fn reads `c`.
#[test]
fn drop_block_expr_returns_7() {
    expect_answer("std/drop/drop_block_expr", 7i32);
}

// T4.5: Drop function parameters. `take(l: Logger)` drops `l` at fn
// end (after returning 42). The outer reads `c` after `take`
// returns and observes the drop side effect.
#[test]
fn drop_fn_param_returns_1() {
    expect_answer("std/drop/drop_fn_param", 1i32);
}

// T4.6: move-aware drops. `let _y: Logger = l;` is a whole-binding
// move of a Drop value — borrowck records it, codegen skips `l`'s
// implicit scope-end drop, and only `_y`'s drop fires.
#[test]
fn drop_moved_returns_0() {
    expect_answer("std/drop/drop_moved", 0i32);
}

#[test]
fn partial_move_of_drop_value_is_rejected() {
    // T4.6: whole-binding moves of a Drop value are now allowed
    // (codegen skips the implicit drop on the moved-from slot).
    // Partial moves remain rejected — Drop's destructor runs over the
    // whole value, so there's no sound way to drop a value with a
    // hole punched in it.
    let err = compile_source(
        "struct Inner { x: u32 }\n\
         struct Outer { i: Inner }\n\
         impl Drop for Outer { fn drop(&mut self) {} }\n\
         fn f() -> u32 {\n\
             let o: Outer = Outer { i: Inner { x: 1 } };\n\
             let i: Inner = o.i;\n\
             0\n\
         }",
    );
    assert!(
        err.contains("type implements `Drop`"),
        "expected drop partial-move error, got: {}",
        err
    );
}

// Pattern interaction: tuple destructure of two Drop values.
// Each destructured binding owns one element and must run its
// Drop at scope end. Both Loggers fire — c ends at 1 + 4 == 5.
#[test]
fn drop_tuple_destructure_runs_both_drops() {
    expect_answer("std/drop/drop_tuple_destructure", 5i32);
}

// Pattern interaction: moving one binding from a destructure
// must not prevent the other from dropping. `take(a)` consumes
// `a` (its drop fires inside `take`'s frame, +1); `_b` then
// drops at the inner block's end (+4). Final c == 5.
#[test]
fn drop_tuple_destructure_partial_move_drops_remaining() {
    expect_answer("std/drop/drop_tuple_destructure_partial_move", 5i32);
}

// Pattern interaction: a let-else pattern binding holding a Drop
// value must drop at the success-path scope end. Pattern matches
// (Some), `_l` binds the Logger, scope ends → drop fires → c=1.
#[test]
fn drop_let_else_match_drops_binding() {
    expect_answer("std/drop/drop_let_else_match", 1i32);
}

// Pattern interaction: when a let-else's pattern doesn't match,
// the scrutinee value still has to be cleaned up. With the Some
// branch never bound and the value a Drop type, the destructor
// must run before/inside the diverging else block. Else-block
// returns 1 explicitly, so a passing test confirms that we got
// to the else block at all (not that we dropped specifically —
// scrutinee-drop-on-no-match is an additional invariant we'd
// want here once let-else's else-block dataflow is in place).
#[test]
fn drop_let_else_no_match_runs_else() {
    expect_answer("std/drop/drop_let_else_no_match", 1i32);
}

// Pattern interaction: a destructured binding must remain
// borrowable. Take `&_a` and `&_b` independently and read both
// through the references — no compile-time conflict, runtime
// reads through addressed slots return the original values.
#[test]
fn drop_pattern_addr_taken_borrows_work() {
    expect_answer("std/drop/drop_pattern_addr_taken", 42i32);
}

// Pattern interaction: drop ordering for destructured bindings is
// reverse declaration order, just like regular `let` bindings. The
// example encodes the visit order in a base-10 counter, so reading
// the final value tells us the order.
#[test]
fn drop_destructure_order_is_reverse_decl() {
    expect_answer("std/drop/drop_destructure_order", 21i32);
}

// Pattern interaction: mutable borrows on disjoint pattern bindings
// work independently. `let (mut a, mut b) = pair;` followed by
// `&mut a` then `&mut b` does not conflict (they're separate
// owned bindings, not a single backing place).
#[test]
fn drop_destructure_mut_borrow_works() {
    expect_answer("std/drop/drop_destructure_mut_borrow", 42i32);
}

// Negative pattern interaction: a destructured binding moved into
// a function call cannot be re-used. `take(a)` consumes `a`; the
// trailing `a.x` read is rejected by borrowck.
#[test]
fn drop_destructure_use_after_move_is_rejected() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         fn take(_f: Foo) {}\n\
         fn answer() -> u32 {\n\
             let pair: (Foo, Foo) = (Foo { x: 1u32 }, Foo { x: 2u32 });\n\
             let (a, _b) = pair;\n\
             take(a);\n\
             a.x\n\
         }",
    );
    assert!(
        err.contains("already moved") || err.contains("after move") || err.contains("moved"),
        "expected use-after-move error, got: {}",
        err
    );
}

// Negative pattern interaction: borrow conflict on a destructured
// binding. `let r = &mut a;` then `let s = &a;` while `r` is still
// live is rejected by borrowck (mutable + shared overlap).
#[test]
fn drop_destructure_borrow_conflict_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 {\n\
             let pair: (u32, u32) = (1u32, 2u32);\n\
             let (mut a, _b) = pair;\n\
             let r = &mut a;\n\
             let s = &a;\n\
             *r + *s\n\
         }",
    );
    assert!(
        err.contains("borrow") || err.contains("conflict"),
        "expected borrow conflict error, got: {}",
        err
    );
}

#[test]
fn drop_and_copy_are_mutually_exclusive() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Copy for Foo {}\n\
         impl Drop for Foo { fn drop(&mut self) {} }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("cannot be implemented") && err.contains("already implements"),
        "expected drop/copy conflict error, got: {}",
        err
    );
}
