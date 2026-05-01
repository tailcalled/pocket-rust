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
