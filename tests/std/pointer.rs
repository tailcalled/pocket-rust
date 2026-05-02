// `std::primitive::pointer` — inherent methods on `*const T` and
// `*mut T` (`byte_add` / `byte_sub` / `byte_offset` / `is_null`).

use super::*;

#[test]
fn pointer_byte_add_returns_42() {
    expect_answer("std/pointer/byte_add", 42u32);
}

#[test]
fn pointer_byte_sub_returns_42() {
    expect_answer("std/pointer/byte_sub", 42u32);
}

#[test]
fn pointer_byte_offset_neg_returns_42() {
    expect_answer("std/pointer/byte_offset_neg", 42u32);
}

#[test]
fn pointer_is_null_true_returns_42() {
    // `(0 as *const T).is_null()` is true.
    expect_answer("std/pointer/is_null_true", 42u32);
}

#[test]
fn pointer_is_null_false_returns_42() {
    // A pointer to a stack-allocated value is non-null.
    expect_answer("std/pointer/is_null_false", 42u32);
}

#[test]
fn pointer_arithmetic_chain_returns_42() {
    // Chained `byte_add` / `byte_sub` / `byte_offset` round-trips
    // back to the original address.
    expect_answer("std/pointer/arithmetic_chain", 42u32);
}

#[test]
fn pointer_mut_byte_add_returns_42() {
    // `*mut T::byte_add` preserves `*mut`-ness; the returned pointer
    // can be written through.
    expect_answer("std/pointer/mut_byte_add", 42u32);
}

#[test]
fn pointer_addr_cast_returns_4() {
    // `*const T as usize` — two consecutive 4-byte allocations should
    // be exactly 4 bytes apart (the bump allocator advances by the
    // requested size). Test the spacing rather than the absolute
    // first-alloc address, which shifts whenever a stdlib string
    // literal lands in the data segment ahead of the heap.
    expect_answer("std/pointer/addr_cast", 4u32);
}
