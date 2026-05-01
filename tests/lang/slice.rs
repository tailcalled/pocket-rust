// Slice fat-ref ABI: `&[T]` (and later `&mut [T]`) flatten to 2 i32s
// (data pointer + length). These tests exercise every place the fat
// ref has to round-trip through codegen — function args, returns,
// struct/enum/tuple fields, if/match arms, generics — to prove the
// ABI is consistent across every value-position.
//
// The slice itself is constructed via `Vec::as_slice()`; the only
// observation is `slice.len()` (the length half of the fat ref). If
// the data-ptr half got corrupted along any path we wouldn't see it
// here, but a length-mismatch surfaces immediately.

use super::*;

#[test]
fn slice_through_fn_arg_returns_42() {
    expect_answer("lang/slice/through_fn", 42u32);
}

#[test]
fn slice_returned_from_fn_returns_42() {
    expect_answer("lang/slice/returned_from_fn", 42u32);
}

#[test]
fn slice_in_struct_field_returns_42() {
    expect_answer("lang/slice/in_struct_field", 42u32);
}

#[test]
fn slice_in_tuple_returns_42() {
    expect_answer("lang/slice/in_tuple", 42u32);
}

#[test]
fn slice_in_enum_variant_returns_42() {
    expect_answer("lang/slice/in_enum_variant", 42u32);
}

#[test]
fn slice_if_result_returns_42() {
    expect_answer("lang/slice/if_result", 42u32);
}

#[test]
fn slice_match_result_returns_42() {
    expect_answer("lang/slice/match_result", 42u32);
}

#[test]
fn slice_through_generic_returns_42() {
    expect_answer("lang/slice/through_generic", 42u32);
}

#[test]
fn slice_is_copy_returns_42() {
    expect_answer("lang/slice/is_copy", 42u32);
}

#[test]
fn slice_spilled_binding_returns_42() {
    expect_answer("lang/slice/spilled_binding", 42u32);
}

#[test]
fn vec_of_slices_returns_42() {
    expect_answer("lang/slice/vec_of_slices", 42u32);
}

#[test]
fn slice_in_struct_by_value_returns_42() {
    expect_answer("lang/slice/struct_by_value", 42u32);
}

#[test]
fn slice_in_option_payload_returns_42() {
    expect_answer("lang/slice/in_option_payload", 42u32);
}

#[test]
fn slice_composite_chain_returns_42() {
    expect_answer("lang/slice/composite_chain", 42u32);
}

#[test]
fn slice_get_in_bounds_returns_42() {
    expect_answer("lang/slice/get_in_bounds", 42u32);
}

#[test]
fn slice_get_out_of_bounds_returns_42() {
    expect_answer("lang/slice/get_out_of_bounds", 42u32);
}

#[test]
fn slice_get_mut_modifies_returns_42() {
    expect_answer("lang/slice/get_mut_modifies", 42u32);
}

#[test]
fn slice_as_mut_slice_writes_returns_42() {
    expect_answer("lang/slice/as_mut_slice_writes", 42u32);
}

// ─── Negative tests ──────────────────────────────────────────────
// Each pins down a misuse-rejection error: the typeck must catch
// the bad shape and surface a recognizable message.

#[test]
fn slice_element_type_mismatch_is_rejected() {
    let err = compile_source(
        "fn count(s: &[u64]) -> u32 { s.len() as u32 } \
         fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(1); \
             count(v.as_slice()) \
         }",
    );
    // Caller passes `&[u32]`, callee expects `&[u64]` — the inner
    // unification has to surface as a type-mismatch.
    assert!(
        err.contains("type mismatch") || err.contains("u32") || err.contains("u64"),
        "expected element-type mismatch error, got: {}",
        err,
    );
}

#[test]
fn passing_vec_where_slice_expected_is_rejected() {
    let err = compile_source(
        "fn count(s: &[u32]) -> u32 { s.len() as u32 } \
         fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(1); \
             count(&v) \
         }",
    );
    // `&Vec<u32>` and `&[u32]` are different types — no implicit
    // deref-coercion path exists.
    assert!(
        err.contains("type mismatch") || err.contains("Vec") || err.contains("["),
        "expected Vec-vs-slice mismatch error, got: {}",
        err,
    );
}

#[test]
fn slice_len_intrinsic_with_wrong_arg_count_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { ¤slice_len::<u32>() as u32 }",
    );
    assert!(
        err.contains("¤slice_len") && err.contains("argument"),
        "expected slice_len arity error, got: {}",
        err,
    );
}

#[test]
fn slice_len_intrinsic_without_type_arg_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(1); \
             ¤slice_len(v.as_slice()) as u32 \
         }",
    );
    assert!(
        err.contains("¤slice_len") && err.contains("type argument"),
        "expected slice_len type-arg error, got: {}",
        err,
    );
}

#[test]
fn make_slice_with_non_pointer_first_arg_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let bad: u32 = 0; \
             let s: &[u32] = unsafe { ¤make_slice::<u32>(bad, 0) }; \
             s.len() as u32 \
         }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("*const u8"),
        "expected raw-ptr mismatch on make_slice arg 0, got: {}",
        err,
    );
}

#[test]
fn get_mut_through_shared_slice_is_rejected() {
    // Calling `&mut self` `get_mut` through a `&[T]` (shared) slice
    // must be rejected — the receiver is `&[T]`, the method needs
    // `&mut [T]`.
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(1); \
             let s: &[u32] = v.as_slice(); \
             match s.get_mut(0) { \
                 Option::Some(r) => *r, \
                 Option::None => 0, \
             } \
         }",
    );
    // Either method-dispatch rejects (no get_mut on &[T] receiver),
    // or it picks up [T]::get_mut and complains about &mut self
    // through &.
    assert!(
        !err.is_empty(),
        "expected get_mut-through-shared-slice rejection, got: {}",
        err,
    );
}

#[test]
fn slice_ptr_intrinsic_with_wrong_arg_count_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let p = ¤slice_ptr::<u32>(); 0 }",
    );
    assert!(
        err.contains("¤slice_ptr") && err.contains("argument"),
        "expected slice_ptr arity error, got: {}",
        err,
    );
}

#[test]
fn slice_mut_ptr_on_shared_slice_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut v: Vec<u32> = Vec::new(); \
             v.push(1); \
             let s: &[u32] = v.as_slice(); \
             let p: *mut u32 = ¤slice_mut_ptr::<u32>(s); \
             0 \
         }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("&mut"),
        "expected slice_mut_ptr-on-shared-slice rejection, got: {}",
        err,
    );
}
