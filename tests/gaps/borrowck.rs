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
