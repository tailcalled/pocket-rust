// `Box<T>` — heap-allocated single-value smart pointer.

use super::*;

// `Box::new(value)` allocates on the heap; `*b` dereferences via
// `<Box<T> as Deref>::deref(&b)`.
#[test]
fn box_basic_returns_42() {
    expect_answer("std/box/box_basic", 42i32);
}

// `*b = value` writes via `<Box<T> as DerefMut>::deref_mut(&mut b)`.
#[test]
fn box_deref_mut_returns_42() {
    expect_answer("std/box/box_deref_mut", 42i32);
}

// `Box::into_inner(b)` extracts T and frees the buffer.
#[test]
fn box_into_inner_returns_42() {
    expect_answer("std/box/box_into_inner", 42i32);
}

// Box drops its inner T at scope-end (destructor + buffer free).
#[test]
fn box_drop_runs_returns_42() {
    expect_answer("std/box/box_drop_runs", 42i32);
}

// `Box::into_raw` / `Box::from_raw` round-trip ownership.
#[test]
fn box_into_raw_from_raw_returns_42() {
    expect_answer("std/box/box_into_raw_from_raw", 42i32);
}

// `Box::as_ptr` and `Box::as_mut_ptr` for non-owning raw views.
#[test]
fn box_as_ptr_returns_42() {
    expect_answer("std/box/box_as_ptr", 42i32);
}

// `Box::leak` returns `&'static mut T` and suppresses Drop.
#[test]
fn box_leak_returns_42() {
    expect_answer("std/box/box_leak", 42i32);
}
