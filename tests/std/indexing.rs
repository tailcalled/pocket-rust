// Index / IndexMut traits + `arr[idx]` syntax. The traits are
// declared in `lib/std/ops.rs`; impls cover `[T]` and `Vec<T>` with
// `Idx = usize` (no generic-trait support yet).
//
// Codegen branches on context: value-position read calls `index`
// then derefs; `&arr[idx]` calls `index`; `&mut arr[idx]` and
// `arr[idx] = val` call `index_mut`.

use super::*;

#[test]
fn vec_read_returns_42() {
    expect_answer("std/indexing/vec_read", 42u32);
}

#[test]
fn vec_write_returns_42() {
    expect_answer("std/indexing/vec_write", 42u32);
}

#[test]
fn slice_read_returns_42() {
    expect_answer("std/indexing/slice_read", 42u32);
}

#[test]
fn borrow_indexed_returns_42() {
    expect_answer("std/indexing/borrow_indexed", 42u32);
}

// Negative: indexing a type with no Index impl.
#[test]
fn index_on_non_indexable_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: u32 = 7; x[0] }",
    );
    assert!(
        err.contains("cannot be indexed") || err.contains("Index"),
        "expected no-Index-impl error, got: {}",
        err
    );
}

// Out-of-bounds index — the bounds check in `Vec::index_*` calls
// `panic!("Vec index out of bounds")`, which the test harness reads
// out of memory and surfaces in the trap's error message.
#[test]
fn vec_index_out_of_bounds_traps_with_message() {
    let err = expect_panic("std/indexing/vec_oob");
    assert!(
        err.contains("Vec index out of bounds"),
        "expected 'Vec index out of bounds' in panic, got: {}",
        err
    );
}

#[test]
fn vec_write_out_of_bounds_traps_with_message() {
    let err = expect_panic("std/indexing/vec_write_oob");
    assert!(
        err.contains("Vec index out of bounds"),
        "expected 'Vec index out of bounds' in panic, got: {}",
        err
    );
}

#[test]
fn slice_index_out_of_bounds_traps_with_message() {
    let err = expect_panic("std/indexing/slice_oob");
    assert!(
        err.contains("slice index out of bounds"),
        "expected 'slice index out of bounds' in panic, got: {}",
        err
    );
}

// Negative: index expression must be `usize`.
#[test]
fn index_with_wrong_idx_type_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(42); \
             let i: u64 = 0; \
             v[i] \
         }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("usize"),
        "expected idx-type error, got: {}",
        err
    );
}
