// Raw pointers (`*const T`, `*mut T`) and `unsafe`. Codegen
// (real-pointer load/store, ref → raw cast, recursive types like linked
// lists) plus `safeck.rs` rejections of raw deref outside `unsafe`.

use super::*;

#[test]
fn deref_const_pointer_returns_42() {
    let bytes = compile_inline(
        "fn answer() -> u32 { \
             let x: u32 = 42; \
             let p: *const u32 = &x as *const u32; \
             unsafe { *p } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

#[test]
fn write_through_mut_pointer_returns_99() {
    let bytes = compile_inline(
        "fn answer() -> u32 { \
             let mut x: u32 = 0; \
             let p: *mut u32 = &mut x as *mut u32; \
             unsafe { *p = 99; } \
             x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 99);
}

#[test]
fn pointer_field_access_returns_7() {
    let bytes = compile_inline(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let pt = Point { x: 7, y: 14 }; \
             let p: *const Point = &pt as *const Point; \
             unsafe { (*p).x } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 7);
}

#[test]
fn pointer_field_write_returns_99() {
    let bytes = compile_inline(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let mut pt = Point { x: 1, y: 2 }; \
             let p: *mut Point = &mut pt as *mut Point; \
             unsafe { (*p).x = 99; } \
             pt.x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 99);
}

#[test]
fn returning_a_raw_pointer_writes_back() {
    let bytes = compile_inline(
        "fn through(p: *mut u32) -> *mut u32 { p } \
         fn answer() -> u32 { \
             let mut x: u32 = 1; \
             let p = through(&mut x as *mut u32); \
             unsafe { *p = 42; } \
             x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

#[test]
fn pointer_in_struct_field_returns_30() {
    let bytes = compile_inline(
        "struct Node { value: u32, next: *const Node } \
         fn answer() -> u32 { \
             let t = Node { value: 30, next: 0 as *const Node }; \
             let h = Node { value: 10, next: &t as *const Node }; \
             unsafe { (*h.next).value } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 30);
}

#[test]
fn linked_list_walk_returns_30() {
    // n1 -> n2 -> n3 -> null. Walk to the third node and read its value.
    let bytes = compile_inline(
        "struct Node { value: u32, next: *const Node } \
         fn answer() -> u32 { \
             let n3 = Node { value: 30, next: 0 as *const Node }; \
             let n2 = Node { value: 20, next: &n3 as *const Node }; \
             let n1 = Node { value: 10, next: &n2 as *const Node }; \
             unsafe { (*(*n1.next).next).value } \
         }",
    );
    assert_eq!(answer_u32(&bytes), 30);
}

#[test]
fn raw_pointer_round_trip_through_function() {
    let bytes = compile_inline(
        "fn make(p: *mut u32) -> *mut u32 { p } \
         fn answer() -> u32 { \
             let mut x: u32 = 0; \
             let q: *mut u32 = make(&mut x as *mut u32); \
             unsafe { *q = 7; } \
             unsafe { *q = 8; } \
             x \
         }",
    );
    assert_eq!(answer_u32(&bytes), 8);
}

#[test]
fn deref_raw_outside_unsafe_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let x: u32 = 1; \
             let p: *const u32 = &x as *const u32; \
             *p \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]
fn write_through_raw_outside_unsafe_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut x: u32 = 1; \
             let p: *mut u32 = &mut x as *mut u32; \
             *p = 99; \
             x \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]
fn raw_pointer_field_access_outside_unsafe_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let p: *const Point = &pt as *const Point; \
             (*p).x \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]
fn raw_pointer_field_write_outside_unsafe_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 } \
         fn answer() -> u32 { \
             let mut pt = Point { x: 1, y: 2 }; \
             let p: *mut Point = &mut pt as *mut Point; \
             (*p).x = 99; \
             pt.x \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}

#[test]
fn unsafe_does_not_extend_outside_block() {
    // Compute the deref inside `unsafe`, then attempt a second deref in
    // the outer scope — that one must fail.
    let err = compile_source(
        "fn answer() -> u32 { \
             let x: u32 = 1; \
             let p: *const u32 = &x as *const u32; \
             let _v = unsafe { *p }; \
             *p \
         }",
    );
    assert!(
        err.contains("unsafe"),
        "expected unsafe-required error, got: {}",
        err
    );
}
