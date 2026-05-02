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

// Range slicing — Index<Range<usize>> / RangeFrom / RangeTo /
// RangeInclusive / RangeToInclusive / RangeFull on `[T]`, `Vec<T>`,
// and `str`. Each impl bounds-checks (and for str, char-boundary-
// checks) before constructing the sub-fat-ref via `¤make_slice` /
// `¤make_str` (or their mutable variants).

// `[T]` slicing — base case + one inclusive variant + full-range
// reborrow + a mutable write through `&mut s[..]`.
#[test]
fn slice_range_returns_50() {
    expect_answer("std/indexing/slice_range", 50u32);
}

#[test]
fn slice_range_inclusive_returns_50() {
    expect_answer("std/indexing/slice_range_inclusive", 50u32);
}

#[test]
fn slice_range_full_returns_100() {
    expect_answer("std/indexing/slice_range_full", 100u32);
}

#[test]
fn slice_range_mut_write_returns_7() {
    expect_answer("std/indexing/slice_range_mut_write", 7u32);
}

// Vec slicing — wrappers around `[T]`'s impls via as_slice /
// as_mut_slice. One bounded form, one unbounded, one full.
#[test]
fn vec_range_returns_50() {
    expect_answer("std/indexing/vec_range", 50u32);
}

#[test]
fn vec_range_from_returns_70() {
    expect_answer("std/indexing/vec_range_from", 70u32);
}

#[test]
fn vec_range_full_returns_100() {
    expect_answer("std/indexing/vec_range_full", 100u32);
}

// `str` slicing — all six range forms exercised. Each checks the
// returned `&str`'s len against the expected byte count.
#[test]
fn str_range_returns_3() {
    expect_answer("std/indexing/str_range", 3u32);
}

#[test]
fn str_range_from_returns_3() {
    expect_answer("std/indexing/str_range_from", 3u32);
}

#[test]
fn str_range_to_returns_3() {
    expect_answer("std/indexing/str_range_to", 3u32);
}

#[test]
fn str_range_inclusive_returns_3() {
    expect_answer("std/indexing/str_range_inclusive", 3u32);
}

#[test]
fn str_range_to_inclusive_returns_3() {
    expect_answer("std/indexing/str_range_to_inclusive", 3u32);
}

#[test]
fn str_range_full_returns_5() {
    expect_answer("std/indexing/str_range_full", 5u32);
}

// Negative slicing: out-of-bounds end traps with the bounds-check
// panic.
#[test]
fn str_range_oob_traps() {
    let err = expect_panic("std/indexing/str_range_oob");
    assert!(
        err.contains("str range end out of bounds"),
        "expected oob panic, got: {}",
        err
    );
}

// Negative slicing: reversed range (start > end) traps.
#[test]
fn str_range_reversed_traps() {
    let err = expect_panic("std/indexing/str_range_reversed");
    assert!(
        err.contains("str range start > end"),
        "expected reversed-range panic, got: {}",
        err
    );
}

// Negative slicing: byte index lands in the middle of a multi-byte
// UTF-8 codepoint (`"a¥b"[0..2]` cuts ¥ in half). Boundary check
// in each `Index<Range*<usize>> for str` impl panics.
#[test]
fn str_range_mid_codepoint_traps() {
    let err = expect_panic("std/indexing/str_range_mid_codepoint");
    assert!(
        err.contains("char boundary"),
        "expected char-boundary panic, got: {}",
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
        err.contains("cannot be indexed") && err.contains("u64"),
        "expected idx-type error, got: {}",
        err
    );
}
