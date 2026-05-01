// `std::option::Option<T>` — `Some(T)` / `None` enum, plus `is_some`,
// `is_none`, `unwrap_or` inherent methods. Construction-and-match
// coverage of the same shape lives in `tests/lang/patterns.rs`; this
// file specifically exercises the stdlib type and its methods.

use super::*;

#[test]
fn option_is_some_returns_42() {
    expect_answer("std/option/is_some", 42u32);
}

#[test]
fn option_is_none_returns_42() {
    expect_answer("std/option/is_none", 42u32);
}

#[test]
fn option_unwrap_or_some_returns_42() {
    expect_answer("std/option/unwrap_or_some", 42u32);
}

#[test]
fn option_unwrap_or_none_returns_42() {
    expect_answer("std/option/unwrap_or_none", 42u32);
}

#[test]
fn option_match_some_returns_42() {
    expect_answer("std/option/match_some", 42u32);
}

#[test]
fn option_match_none_returns_42() {
    expect_answer("std/option/match_none", 42u32);
}

#[test]
fn option_nested_returns_42() {
    expect_answer("std/option/nested", 42u32);
}

#[test]
fn option_and_some_returns_42() {
    expect_answer("std/option/and_some", 42u32);
}

#[test]
fn option_and_none_returns_42() {
    expect_answer("std/option/and_none", 42u32);
}

#[test]
fn option_or_some_returns_42() {
    expect_answer("std/option/or_some", 42u32);
}

#[test]
fn option_or_none_returns_42() {
    expect_answer("std/option/or_none", 42u32);
}

#[test]
fn option_xor_one_returns_42() {
    expect_answer("std/option/xor_one", 42u32);
}

#[test]
fn option_xor_both_returns_42() {
    expect_answer("std/option/xor_both", 42u32);
}

#[test]
fn option_xor_neither_returns_42() {
    expect_answer("std/option/xor_neither", 42u32);
}

#[test]
fn option_flatten_some_returns_42() {
    expect_answer("std/option/flatten_some", 42u32);
}

#[test]
fn option_flatten_inner_none_returns_42() {
    expect_answer("std/option/flatten_inner_none", 42u32);
}

#[test]
fn option_flatten_outer_none_returns_42() {
    expect_answer("std/option/flatten_outer_none", 42u32);
}
