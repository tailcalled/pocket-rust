// Borrow-check conflict tests. Anything that the borrow checker
// rejects (mutable+anything, write-while-borrowed, move-after-move,
// etc.) lives here. Positive ref/lifetime tests are in
// `tests/lang/references.rs`.

use super::*;

#[test]
fn duplicate_partial_move_is_rejected() {
    // `o.p` is non-Copy (struct); moving it twice is an error.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { p: Inner, q: Inner }\n\
         fn f(o: Outer) -> Outer { Outer { p: o.p, q: o.p } }",
    );
    assert!(
        err.contains("already moved"),
        "expected move error, got: {}",
        err
    );
}

#[test]
fn whole_after_partial_move_is_rejected() {
    // `p.a` is non-Copy; moving it then re-using it errors.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Pair { a: Inner, b: Inner }\n\
         fn use_pair(p: Pair) -> usize { p.a.v }\n\
         fn f(p: Pair) -> usize { use_pair(Pair { a: p.a, b: p.a }) }",
    );
    assert!(
        err.contains("already moved"),
        "expected move error, got: {}",
        err
    );
}

#[test]
fn move_while_borrowed_is_rejected() {
    // Two args of the same call: a borrow of `o` and a move out of
    // `o.y` (non-Copy). The borrow is still active when the second
    // arg is evaluated, so the move conflicts.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { x: Inner, y: Inner }\n\
         fn use_borrow(o: &Outer, q: Inner) -> usize { q.v }\n\
         fn bad(o: Outer) -> usize { use_borrow(&o, o.y) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn borrow_after_move_is_rejected() {
    // Move `o.x` (non-Copy) in arg 0, then try to borrow `&o` in arg
    // 1 — `o` is partially moved, so the borrow errors.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { x: Inner, y: usize }\n\
         fn y_of(o: &Outer) -> usize { o.y }\n\
         fn first(a: Inner, b: usize) -> usize { a.v }\n\
         fn bad(o: Outer) -> usize { first(o.x, y_of(&o)) }",
    );
    assert!(
        err.contains("moved") || err.contains("borrowed"),
        "expected borrow-after-move error, got: {}",
        err
    );
}

#[test]
fn move_out_of_borrow_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Rect { tl: Point, br: Point }\nfn whoops(r: &Rect) -> Point { r.tl }",
    );
    assert!(
        err.contains("cannot move out of borrow"),
        "expected move-out-of-borrow error, got: {}",
        err
    );
}

#[test]
fn method_call_borrow_outlives_source_is_rejected() {
    // Borrow returned by `&self -> &u32` method should propagate the
    // receiver's borrow, blocking subsequent moves of the receiver.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         impl Point { fn x_ref(&self) -> &u32 { &self.x } }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let r: &u32 = pt.x_ref(); \
             let q = pt; \
             *r \
         }",
    );
    assert!(
        err.contains("cannot move") && err.contains("borrowed"),
        "expected propagated-method-borrow error, got: {}",
        err
    );
}

#[test]
fn returned_borrow_outlives_source_is_rejected() {
    // Borrowck propagates the input borrow through the call: `r`
    // carries a borrow on `pt`, so moving `pt` afterward must
    // conflict.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn borrow_field(pt: &Point) -> &u32 { &pt.x }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let r: &u32 = borrow_field(&pt); \
             let q = pt; \
             *r \
         }",
    );
    assert!(
        err.contains("cannot move") && err.contains("borrowed"),
        "expected propagated-borrow error, got: {}",
        err
    );
}

#[test]
fn borrow_through_inner_block_blocks_outer_move() {
    // The borrow `&pt1` is created inside the inner block, but the
    // block returns it (as `pt3`) so the borrow ends up bound to
    // `pt2`. A subsequent move of `pt1` must be rejected — `pt2`
    // would otherwise be a dangling reference.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn x_of(p: &Point) -> usize { p.x }\nfn f() -> usize { let pt1 = Point { x: 42, y: 0 }; let pt2 = { let pt3 = &pt1; pt3 }; let invalid = pt1; x_of(pt2) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn borrow_of_subfield_blocks_parent_move() {
    // `&p.x` borrows the subfield, leaving `p` with a borrowed sub-
    // place. Trying to move `p` whole while `r` is still live then
    // has to fail.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f(p: Point) -> usize { let r = &p.x; let q = p; *r }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn two_mut_borrows_of_same_place_conflict() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn take(a: &mut Point, b: &mut Point) -> u32 { a.x }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; take(&mut p, &mut p) }",
    );
    assert!(
        err.contains("already borrowed") || err.contains("borrowed"),
        "expected mut/mut borrow conflict, got: {}",
        err
    );
}

#[test]
fn shared_and_mut_borrow_conflict() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn take(a: &mut Point, b: &Point) -> u32 { a.x }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; take(&mut p, &p) }",
    );
    assert!(
        err.contains("already borrowed") || err.contains("borrowed"),
        "expected mut/shared borrow conflict, got: {}",
        err
    );
}

#[test]
fn move_through_combined_borrow_is_rejected() {
    // `longer<'a>(x, y)` ties both args to the result; moving either
    // while the result is live conflicts.
    let err = compile_source(
        "struct B { v: u32 }\nfn longer<'a>(x: &'a u32, y: &'a u32) -> &'a u32 { x }\nfn f() -> u32 { let a: B = B { v: 1 }; let b: B = B { v: 2 }; let r: &u32 = longer(&a.v, &b.v); let b2: B = b; *r }",
    );
    assert!(
        err.contains("while it is borrowed") || err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn move_through_struct_field_borrow_is_rejected() {
    // Moving the place borrowed by a struct's ref field is rejected
    // as long as the wrapper is still live.
    let err = compile_source(
        "struct Inner { x: u32 }\nstruct Wrapper<'a> { r: &'a Inner }\nfn f() -> u32 { let i: Inner = Inner { x: 1 }; let w: Wrapper<'_> = Wrapper { r: &i }; let i2: Inner = i; let r: &Inner = w.r; r.x }",
    );
    assert!(
        err.contains("while it is borrowed") || err.contains("borrowed"),
        "expected move-while-field-borrowed error, got: {}",
        err
    );
}

#[test]
fn assignment_while_borrowed_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn x_of(p: &Point) -> u32 { p.x }\nfn use_borrow(p: &Point, q: u32) -> u32 { q }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; let r = &p; use_borrow(r, { p.x = 99; p.x }) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected borrow-conflict error, got: {}",
        err
    );
}

#[test]
fn nested_borrow_blocks_conflicting_mut_borrow() {
    // Nested per-slot tracking. The inner borrow of `x` lives in `o`'s
    // field path `["i","r"]`. Taking `&mut x` while `o` is still live
    // (the tail reads `o.i.r`) must conflict.
    let err = compile_source(
        "struct Inner<'a> { r: &'a u32 }\n\
         struct Outer<'a> { i: Inner<'a> }\n\
         fn f() -> u32 {\n\
             let mut x: u32 = 5;\n\
             let o: Outer = Outer { i: Inner { r: &x } };\n\
             let _m: &mut u32 = &mut x;\n\
             *o.i.r\n\
         }",
    );
    assert!(
        err.contains("already borrowed"),
        "expected borrow-conflict error, got: {}",
        err
    );
}

#[test]
fn read_after_maybe_moved_in_if_is_rejected() {
    // Drop binding moved in then-arm only; reading after the if is a
    // read of a MaybeMoved place, which borrowck rejects.
    let err = compile_source(
        "struct L { p: *mut u32 }\n\
         impl Drop for L { fn drop(&mut self) { unsafe { *self.p = 1; } } }\n\
         fn take(_l: L) -> u32 { 0 }\n\
         fn answer() -> u32 {\n\
             let mut c: u32 = 5;\n\
             let l: L = L { p: &mut c as *mut u32 };\n\
             let _v: u32 = if true { take(l) } else { 0 };\n\
             let _x: L = l;\n\
             0\n\
         }",
    );
    assert!(
        err.contains("moved") || err.contains("already"),
        "expected read-after-MaybeMoved error, got: {}",
        err
    );
}
