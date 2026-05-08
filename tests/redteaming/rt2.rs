// Round 2 of red-team findings — bugs surfaced by the compiler's own
// dead-code / unused-variable warnings. As with `rt1.rs`, **every
// test in this file is expected to fail** today: the failure *is* the
// surfaced bug.
//
// rt2 differs from rt1 in posture: rt1's bugs are "valid program
// rejected"; rt2's are typically "invalid program accepted" (or a
// rejection that happens for the wrong reason). The right shape of
// these tests is therefore an assertion that compilation FAILS with
// a particular diagnostic — not that it succeeds and yields 42.

use super::*;

// PROBLEM 1: `dispatch_method_through_trait`
// (src/typeck/methods.rs) handles `TraitReceiverShape::Move` with
// only one rejection branch:
//
//     Some(TraitReceiverShape::Move) => {
//         if recv_through_mut_ref {
//             return Err(... "cannot move out of `&mut T`" ...);
//         }
//         ReceiverAdjust::Move
//     }
//
// `recv_through_shared_ref` (computed two lines above, on line 136)
// is never read. So `&T` receivers — which would move out of a
// *shared* reference — fall straight through to `ReceiverAdjust::Move`
// and the call is accepted. Real Rust rejects this with E0507.
//
// At runtime the consequences are observable: the call is codegenned
// with `recv_adjust = Move` (no autoref, no autoderef), so the recv
// expression's value (an `i32` shadow-stack address) is passed as
// `self: T`. For `impl Take for u32 { fn take(self) -> u32 { self } }`
// the function returns its `self` arg — i.e. it returns the pointer,
// not the dereferenced value. So `through_ref(&42u32)` returns the
// shadow-stack slot's address, not 42.
//
// Why it's architectural-ish (vs. just a missing branch): the dead
// `recv_through_shared_ref` variable is a left-over from a refactor
// that delegated `check_method_call_symbolic` → `dispatch_method_…`.
// The same pair of variables is dead in `check_method_call_symbolic`
// (lines 29–30) and live-but-incomplete in `dispatch_method_…`
// (lines 135–136). The pattern says: receiver-shape gating logic was
// moved between functions but the SharedRef branch never made it
// across.
//
// Fix shape: extend the `Move` arm to reject `recv_through_shared_ref`
// the same way it rejects `recv_through_mut_ref`. Both are "moving
// out of a place behind a reference"; the only difference is the
// reference's mutability. The error message should mention the
// receiver kind (`&T` vs `&mut T`).
#[test]
fn problem_1_move_self_through_shared_ref_rejected() {
    let err = try_compile_example(
        "redteaming/rt2/move_self_through_shared_ref",
        "lib.rs",
    )
    .err()
    .expect("expected compile error: cannot move out of `&T`");
    assert!(
        err.contains("cannot move") || err.contains("shared reference"),
        "expected move-out-of-`&T` diagnostic, got: {}",
        err,
    );
}

// PROBLEM 2: pocket-rust represents every Narrow32 integer (u8/i8/
// u16/i16/u32/i32/usize/isize) as a wasm i32 in wasm-locals and
// emits raw `I32Add`/`I32Sub`/`I32Mul` for arithmetic — never
// masking the result back to the source type's bit width. So
// `255u8 + 43u8` produces 298 in pocket-rust where real Rust
// (release) wraps to 42 and real Rust (debug) panics with overflow.
//
// Why it's architectural: the narrow-int width invariant is
// enforced ad hoc — `I32Store8` truncates at memory store time, but
// every other code path treats narrow values as full-width i32. The
// compiler never inserts an explicit mask after arithmetic, after
// casts (see `problem_3`), or after function calls returning narrow
// types. Wherever a narrow value flows entirely through wasm-locals,
// it carries arbitrary high bits.
//
// Fix shape (landed): centralized into `emit_narrow_normalize(ctx,
// kind: &IntKind)` (src/codegen.rs). Establishes the invariant
// "narrow-typed wasm-locals carry in-range values" with enforcement
// at the producer sites — binop result emission (gated by
// `op_can_overflow_narrow`) and the `Narrow32 → Narrow32` arm of
// `emit_int_to_int_cast` when the target is strictly narrower
// (closes problem_3 too). Other producers (literals — typeck range-
// checked; function returns — masked at the callee; bitwise ops,
// compares, memory loads — width-correct already) don't need it.
#[test]
fn problem_2_narrow_int_arithmetic_does_not_wrap() {
    expect_answer("redteaming/rt2/narrow_int_add_no_wrap", 42u32);
}

// PROBLEM 3: `as` casts within the Narrow32 integer class emit zero
// wasm instructions. `emit_int_to_int_cast` (src/codegen.rs:3021)
// matches on `(src_class, tgt_class)` and only emits a wasm op when
// the classes differ. Since u8/i8/…/i32/usize/isize all share the
// `Narrow32` class, casting *between any of them* (e.g. `u32 → u8`,
// `i16 → u8`, `i32 → i8`) is a no-op. So `298u32 as u8` stays 298,
// `(-214i32) as u8` stays the i32 representation of -214, etc.
//
// Why it's architectural: same root cause as problem 2 — the
// Narrow32 width invariant lives only in the type system, not in
// the wasm representation. The cast layer trusts the source value
// to already be masked; the source is rarely masked because no other
// layer ever masks it.
//
// Fix shape (landed): same as problem 2 — `emit_int_to_int_cast`'s
// `Narrow32 → Narrow32` arm calls `emit_narrow_normalize(ctx, tgt)`
// whenever `narrow_bit_width(tgt) < narrow_bit_width(src)`. Same-
// class widening (e.g. `u8 → u32`) stays a no-op, relying on the
// invariant problem 2's fix establishes.
#[test]
fn problem_3_narrow_int_cast_does_not_truncate() {
    expect_answer("redteaming/rt2/narrow_int_cast_no_truncate", 42u32);
}

// PROBLEM 4: a layered consequence of problems 2 and 3 — the same
// `u8` computation produces *different observable values* depending
// on whether the binding ever has its address taken.
//
// `let a: u8 = 255u8 + 43u8;` lives in a wasm-local; the arithmetic
// gap (problem 2) leaves `a == 298`. `let b: u8 = 255u8 + 43u8; let
// r = &b;` forces `b` into shadow-stack `Memory` storage; the layout
// pass picks `Storage::Memory`; codegen materializes `b` via
// `I32Store8` (which silently truncates to one byte) and reads it
// back via `I32Load8U` (which zero-extends). So `*r == 42`.
//
// Why it's architectural — and worse than the sum of problems 2
// and 3: `Storage::Local` vs `Storage::Memory` was designed as an
// invisible representation choice driven by escape analysis. With
// the narrow-int gap, the choice leaks into program semantics. The
// program a developer writes can produce different answers based on
// whether they incidentally took a reference somewhere, with no
// indication in the source that `&b` changed `b`'s value.
//
// Fix shape: covered by problems 2 and 3. Once arithmetic and casts
// always mask back to the type's width, both storage paths carry
// the same value.
#[test]
fn problem_4_narrow_int_local_vs_memory_storage_diverges() {
    expect_answer("redteaming/rt2/narrow_int_storage_divergence", 42u32);
}

// PROBLEM 5: pocket-rust skips drop glue for any aggregate that
// doesn't itself implement `Drop`. `compute_drop_action`
// (src/layout.rs) returns `Skip` whenever `is_drop(ty, traits)` is
// false, and `is_drop` (src/typeck/types.rs:771) only checks for a
// *direct* `impl Drop for T` — never recursing into struct fields,
// enum variant payloads, or tuple elements.
//
// So `struct Pair { a: Tracker, b: Tracker }` — where `Tracker`
// implements `Drop` but `Pair` doesn't — produces NO destruction at
// scope end. Both Trackers leak. Same for `(Tracker, Tracker)` and
// for `enum E { Both(Tracker, Tracker) }`. The same trap fires when
// the wrapping type *does* implement `Drop`: only the user's
// `drop(&mut self)` runs, never the field destructors.
//
// Why architectural: drop glue is foundational. Every owning
// container in real Rust depends on it — `Vec<T>` frees its
// allocation by way of `Vec`'s Drop dropping the heap memory + the
// element drops being run individually. Without drop glue, every
// stdlib container that holds heap resources leaks them; every
// user struct holding a `Box<T>` / `Vec<T>` / file handle leaks
// when the wrapper goes out of scope. The current behavior is
// silent: nothing in the type system or in any error message hints
// that the inner Tracker was missed.
//
// Fix shape: introduce a `needs_drop(ty, structs, enums, traits)`
// that is true for any type with a direct Drop impl OR any
// aggregate containing a `needs_drop` sub-field. Make
// `compute_drop_action` consult `needs_drop`. Replace the single
// `Drop::drop` call in `emit_drop_call_for_local` with a recursive
// drop-walker: call the user's `Drop::drop` first if present, then
// walk the type's structure (struct fields in declaration order;
// enum payloads after a discriminant test; tuple elements in
// position order), recursively calling drop on every needs_drop
// leaf. Mirror real Rust's drop order conventions.
#[test]
fn problem_5_aggregate_field_drop_glue_missing() {
    expect_answer("redteaming/rt2/struct_field_drop_glue_missing", 42u32);
}
