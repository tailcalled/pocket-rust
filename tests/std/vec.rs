// `std::vec::Vec<T>` — heap-allocated, dynamically resizable array.
// Coverage walks the public surface end-to-end: push/pop, growth past
// the initial capacity, get/get_mut, clear, and Drop running per
// element when the Vec itself drops.

use super::*;

#[test]
fn vec_push_pop_basic_returns_42() {
    expect_answer("std/vec/push_pop_basic", 42u32);
}

#[test]
fn vec_push_past_initial_capacity_returns_sum_21() {
    // 1+2+3+4+5+6 = 21
    expect_answer("std/vec/push_pop_grow", 21u32);
}

#[test]
fn vec_pop_on_empty_returns_42() {
    expect_answer("std/vec/empty_pop_returns_none", 42u32);
}

#[test]
fn vec_clear_then_push_returns_42() {
    expect_answer("std/vec/clear_then_push", 42u32);
}

#[test]
fn vec_get_in_bounds_returns_42() {
    expect_answer("std/vec/get_in_bounds", 42u32);
}

#[test]
fn vec_get_out_of_bounds_returns_42() {
    expect_answer("std/vec/get_out_of_bounds", 42u32);
}

#[test]
fn vec_get_mut_modifies_returns_42() {
    expect_answer("std/vec/get_mut_modifies", 42u32);
}

// `vec[i] += rhs` — Index lowering desugars `vec[i]` to a synth
// `IndexMut::index_mut(&mut vec, i)` MethodCall (since the outer
// `add_assign` autoref picks BorrowMut), and codegen extracts the
// place without flattening away the inner MethodCall. Used to silently
// miscompile through `mono_expr_as_place`'s clone — see the example's
// header comment.
#[test]
fn vec_index_compound_assign_returns_42() {
    expect_answer("std/vec/index_compound_assign", 42u32);
}

#[test]
fn vec_drop_runs_on_elements_returns_3() {
    expect_answer("std/vec/drop_runs_on_elements", 3u32);
}

#[test]
fn vec_inference_challenge_returns_42() {
    expect_answer("std/vec/inference_challenge", 42u32);
}

#[test]
fn vec_inference_nested_returns_42() {
    expect_answer("std/vec/inference_nested", 42u32);
}

#[test]
fn vec_as_slice_len_returns_3() {
    expect_answer("std/vec/as_slice_len", 3u32);
}
