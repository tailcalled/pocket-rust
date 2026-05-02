// `while` loops, `break`, `continue`, plus the labeled forms.

use super::*;

#[test]
fn while_simple_count_returns_5() {
    expect_answer("lang/while_loops/while_simple_count", 5u32);
}

#[test]
fn while_two_vars_returns_15() {
    expect_answer("lang/while_loops/while_two_vars", 15u32);
}

#[test]
fn while_counted_returns_45() {
    // 0+1+2+...+9 = 45
    expect_answer("lang/while_loops/while_counted", 45u32);
}

#[test]
fn while_break_returns_42() {
    expect_answer("lang/while_loops/while_break", 42u32);
}

#[test]
fn while_continue_returns_50() {
    // sum 1..=10 minus 5 = 55 - 5 = 50
    expect_answer("lang/while_loops/while_continue", 50u32);
}

#[test]
fn while_nested_returns_12() {
    // 4 outer * 3 inner = 12
    expect_answer("lang/while_loops/while_nested", 12u32);
}

#[test]
fn while_labeled_break_returns_42() {
    // i goes 0,1,2,3 (4 iters), each inner runs full 10 → 40.
    // Then i=4: inner runs j=0 (count=41), j=1 (count=42), j=2 →
    // break 'outer.
    expect_answer("lang/while_loops/while_labeled_break", 42u32);
}

#[test]
fn while_labeled_continue_returns_3() {
    // outer i=1,2,3 (3 iters). Each inner: j=1 → count=1, j=2 →
    // continue 'outer (skips count=100). 3 outer iters × 1 increment
    // each = 3.
    expect_answer("lang/while_loops/while_labeled_continue", 3u32);
}

// `break` typed as `!` so it can sit in an `if` arm whose other arm
// yields a real value. Without the never type this would fail to
// type-check.
#[test]
fn while_break_in_if_arm_returns_42() {
    expect_answer("lang/while_loops/while_break_in_if_arm", 42u32);
}

// Same shape with `continue` — diverges, allowing the if's type to
// be the else arm's type.
#[test]
fn while_continue_in_if_arm_returns_42() {
    expect_answer("lang/while_loops/while_continue_in_if_arm", 42u32);
}

// `break` in a `match` arm. Match's type is the other arm's u32.
#[test]
fn while_break_in_match_arm_returns_42() {
    expect_answer("lang/while_loops/while_break_in_match_arm", 42u32);
}

// Negative: `break` outside any loop. Same rule as before — the
// loop-target lookup fails — but pin it down explicitly.
#[test]
fn break_outside_loop_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let _x: u32 = if true { break } else { 42 }; 0 }",
    );
    assert!(
        err.contains("break") && (err.contains("outside") || err.contains("not in")),
        "expected break-outside-loop error, got: {}",
        err
    );
}
