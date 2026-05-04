// `match` and `if let` patterns: variant destructuring, or-patterns,
// ranges, at-bindings, ref-patterns, guards.

use super::*;

#[test]
fn match_int_returns_42() {
    expect_answer("lang/patterns/match_int", 42u32);
}

#[test]
fn match_ergonomics_auto_peel_variant() {
    expect_answer("lang/patterns/auto_peel_variant", 42u32);
}

#[test]
fn match_ergonomics_auto_peel_struct() {
    expect_answer("lang/patterns/auto_peel_struct", 42u32);
}

#[test]
fn match_ergonomics_auto_peel_tuple() {
    expect_answer("lang/patterns/auto_peel_tuple", 42u32);
}

#[test]
fn match_ergonomics_auto_peel_mut() {
    expect_answer("lang/patterns/auto_peel_mut", 42u32);
}

#[test]
fn match_ergonomics_auto_peel_double() {
    expect_answer("lang/patterns/auto_peel_double", 42u32);
}

#[test]
fn match_ergonomics_explicit_ref_still_works() {
    // The pre-ergonomics explicit `&pat` form must keep working —
    // stdlib (option.rs / result.rs) is written in that style.
    expect_answer("lang/patterns/explicit_ref_resets_mode", 42u32);
}

#[test]
fn match_enum_unit_returns_42() {
    expect_answer("lang/patterns/match_enum_unit", 42u32);
}

#[test]
fn match_enum_tuple_returns_42() {
    expect_answer("lang/patterns/match_enum_tuple", 42u32);
}

#[test]
fn match_enum_struct_returns_42() {
    expect_answer("lang/patterns/match_enum_struct", 42u32);
}

#[test]
fn match_or_returns_42() {
    expect_answer("lang/patterns/match_or", 42u32);
}

#[test]
fn match_range_returns_42() {
    expect_answer("lang/patterns/match_range", 42u32);
}

#[test]
fn match_recursive_returns_42() {
    expect_answer("lang/patterns/match_recursive", 42u32);
}

#[test]
fn match_at_binding_returns_42() {
    expect_answer("lang/patterns/match_at_binding", 42u32);
}

#[test]
fn match_tuple_scrut_returns_42() {
    expect_answer("lang/patterns/match_tuple_scrut", 42u32);
}

#[test]
fn match_returns_enum_returns_42() {
    expect_answer("lang/patterns/match_returns_enum", 42u32);
}

#[test]
fn match_ref_pat_returns_42() {
    expect_answer("lang/patterns/match_ref_pat", 42u32);
}

#[test]
fn match_ref_variant_returns_42() {
    expect_answer("lang/patterns/match_ref_variant", 42u32);
}

#[test]
fn match_ref_bind_returns_42() {
    expect_answer("lang/patterns/match_ref_bind", 42u32);
}

#[test]
fn match_struct_scrut_returns_42() {
    expect_answer("lang/patterns/match_struct_scrut", 42u32);
}

#[test]
fn match_struct_field_pat_returns_42() {
    expect_answer("lang/patterns/match_struct_field_pat", 42u32);
}

#[test]
fn match_tuple_lit_pat_returns_42() {
    expect_answer("lang/patterns/match_tuple_lit_pat", 42u32);
}

#[test]
fn match_ref_locals_returns_42() {
    expect_answer("lang/patterns/match_ref_locals", 42u32);
}

#[test]
fn match_double_use_returns_82() {
    // Each pattern binding `a` and `b` is `u32` (Copy). Borrowck
    // must treat reads of these as non-moving — without proper type
    // propagation through patterns, the second read of `a` would
    // error "already moved".
    expect_answer("lang/patterns/match_double_use", 82u32);
}

#[test]
fn match_move_payload_returns_42() {
    // Pattern binding moves a non-Copy value out of an enum
    // payload: `Wrap::Some(inner)` captures `inner: Owned` by
    // value, then the arm body returns it.
    expect_answer("lang/patterns/match_move_payload", 42u32);
}

#[test]
fn match_guard_returns_42() {
    // First guard `n < 10` fails on 42; second guard `n < 50`
    // succeeds; arm body returns `n` (= 42).
    expect_answer("lang/patterns/match_guard", 42u32);
}

#[test]
fn match_mut_pat_borrow_returns_42() {
    // `mut n` pattern binding + `&mut n` later: escape analysis
    // marks the binding as addressed, so bind_pattern_value spills
    // it to a shadow-stack slot from the start. `*r = 42` writes
    // through the slot's address; reading `n` afterwards reads the
    // same slot and sees 42.
    expect_answer("lang/patterns/match_mut_pat_borrow", 42u32);
}

#[test]
fn if_let_basic_returns_42() {
    expect_answer("lang/patterns/if_let_basic", 42u32);
}

#[test]
fn if_let_no_else_returns_42() {
    expect_answer("lang/patterns/if_let_no_else", 42u32);
}

#[test]
fn if_let_else_returns_42() {
    expect_answer("lang/patterns/if_let_else", 42u32);
}

#[test]
fn if_let_chain_returns_42() {
    expect_answer("lang/patterns/if_let_chain", 42u32);
}

#[test]
fn match_non_exhaustive_is_rejected() {
    let err = compile_source(
        "fn f() -> u32 { let x: u32 = 5; match x { 0 => 1, 1 => 2 } }",
    );
    assert!(
        err.contains("non-exhaustive"),
        "expected non-exhaustive error, got: {}",
        err
    );
}

#[test]
fn match_pattern_move_then_reuse_is_rejected() {
    // The pattern binds a non-Copy `Owned` payload by value; the
    // arm body uses it twice. Borrowck must catch the second use as
    // a move-after-move.
    let err = compile_source(
        "struct Owned { n: u32 }\n\
         enum Wrap { Some(Owned), None }\n\
         fn use_twice(o: Owned) -> u32 { o.n }\n\
         fn f() -> u32 {\n\
             let w: Wrap = Wrap::Some(Owned { n: 42 });\n\
             match w {\n\
                 Wrap::Some(inner) => use_twice(inner) + use_twice(inner),\n\
                 Wrap::None => 0,\n\
             }\n\
         }",
    );
    assert!(
        err.contains("already moved"),
        "expected move-after-move error, got: {}",
        err
    );
}

#[test]
fn match_partial_move_invalidates_scrutinee() {
    // Binding a non-Copy variant payload by value partial-moves the
    // scrutinee place. Subsequent use of the scrutinee must error.
    let err = compile_source(
        "struct Owned { n: u32 }\n\
         enum Wrap { Some(Owned), None }\n\
         fn use_owned(o: Owned) -> u32 { o.n }\n\
         fn use_wrap(w: Wrap) -> u32 { 0 }\n\
         fn f() -> u32 {\n\
             let w: Wrap = Wrap::Some(Owned { n: 42 });\n\
             let n: u32 = match w {\n\
                 Wrap::Some(inner) => use_owned(inner),\n\
                 Wrap::None => 0,\n\
             };\n\
             use_wrap(w) + n\n\
         }",
    );
    assert!(
        err.contains("already moved") || err.contains("moved"),
        "expected partial-move-of-scrutinee error, got: {}",
        err
    );
}

#[test]
fn match_ref_binding_outstanding_borrow_blocks_mut() {
    // `ref rx` against a place creates a borrow of that place. While
    // `rx` is live, taking `&mut p` should conflict.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn set(p: &mut Point) { }\n\
         fn f() -> u32 {\n\
             let mut p: Point = Point { x: 1, y: 2 };\n\
             let n: u32 = match p {\n\
                 Point { x: ref rx, y } => { let _m = &mut p; *rx + y }\n\
             };\n\
             n\n\
         }",
    );
    assert!(
        err.contains("borrow") || err.contains("conflict") || err.contains("already moved"),
        "expected borrow conflict, got: {}",
        err
    );
}
