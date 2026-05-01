// `std::marker::Copy` — primitive impls, generic Copy bounds,
// user-struct `impl Copy`, Drop/Copy mutual exclusion.

use super::*;

#[test]
fn copy_double_use_returns_7() {
    expect_answer("std/copy/copy_double_use", 7i32);
}

#[test]
fn copy_generic_with_bound_returns_42() {
    // `impl<T: Copy> Copy for Wrap<T> {}` validates: the bound makes
    // `Param(T)` Copy so the `inner: T` field passes the field-Copy
    // check.
    expect_answer("std/copy/copy_generic_with_bound", 42i32);
}

#[test]
fn copy_param_via_bound_returns_42() {
    // In a generic body with `T: Copy`, reading `t` after `let s = t`
    // is a value copy (not a move) because the bound makes `Param(T)`
    // Copy.
    expect_answer("std/copy/copy_param_via_bound", 42i32);
}

#[test]
fn copy_user_struct_returns_42() {
    // User-defined `impl Copy for Pt {}`. Reading `p` after `let q =
    // p` should be allowed since Pt is Copy.
    expect_answer("std/copy/copy_user_struct", 42i32);
}

#[test]
fn copy_mut_ref_not_copy_returns_42() {
    // `&mut T` is NOT Copy — assigning a mut-ref to another binding
    // moves it (preserves exclusivity). Reading the original after
    // move would be rejected; this test passes the move via the new
    // binding.
    expect_answer("std/copy/copy_mut_ref_not_copy", 42i32);
}

#[test]
fn generic_copy_impl_without_bound_is_rejected() {
    let err = compile_source(
        "struct Wrap<T> { inner: T }\n\
         impl<T> Copy for Wrap<T> {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("the trait `Copy` is not implemented"),
        "expected non-Copy field error, got: {}",
        err
    );
}

#[test]
fn impl_copy_for_struct_with_non_copy_field_is_rejected() {
    let err = compile_source(
        "struct Inner { x: u32 }\n\
         struct Outer { i: Inner }\n\
         impl Copy for Outer {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("the trait `Copy` is not implemented"),
        "expected non-Copy-field error, got: {}",
        err
    );
}
