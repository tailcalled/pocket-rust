// Borrow-check gaps: cases pocket-rust accepts today but rustc
// rejects, or pocket-rust rejects with a confusing error.

use super::*;

// rustc rejects this with E0716 ("temporary value dropped while
// borrowed"): `&(x + 1)` borrows a temporary that's freed at the end
// of the let-statement, but `*w` reads from it on the next loop
// iteration. pocket-rust currently accepts because borrowck doesn't
// track borrows of temporaries — codegen materializes all temps into
// shadow-stack slots that live until function exit, masking the
// dangle at runtime.
//
// Fix: borrowck should model temp scopes (statement-end by default),
// track borrows of temps, and reject any borrow whose live use
// outlives the temp's scope. Same family as the open "temp
// destructors" deferral.
// rustc rejects this with E0507: `(*o).p` projects through a borrow,
// so moving the inner `Inner` (non-Copy) field steals from the
// caller's `Outer`. pocket-rust accepts: the borrowck
// `move_traverses_borrow` check is intentionally narrow (last
// projection on the root only) — extending it to handle multi-step
// chains needs the struct-table threaded into moves.rs so the walker
// can resolve field types past the outer Deref.
//
// Fix: thread `&StructTable` / `&EnumTable` through `moves::analyze`,
// extend `move_traverses_borrow` to walk multi-step chains tracking a
// "behind borrow" flag, and reject the `(*o).p` case along with
// `o.x.p`-style multi-Field chains.
#[test]
fn move_through_explicit_deref_then_field_is_rejected() {
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { p: Inner }\n\
         impl Inner { fn consume(self) -> usize { self.v } }\n\
         fn whoops(o: &Outer) -> usize { (*o).p.consume() }",
    );
    assert!(
        err.contains("move") || err.contains("borrow"),
        "expected move-out-of-borrow error, got: {}",
        err,
    );
}

#[test]
fn borrow_of_temp_in_tuple_outliving_statement_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { \
             let mut x: u32 = 0; \
             while x < 10u32 { \
                 let w = ({ x = x + 1u32; x }, &(x + 1u32)).1; \
                 x = *w; \
             } \
             x \
         }",
    );
    assert!(
        err.contains("temporary") || err.contains("does not live long enough"),
        "expected a temp-lifetime borrow error, got: {}",
        err
    );
}
