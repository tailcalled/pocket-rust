// `*p += N` for `p: &mut u32` should work — `*p` is a mutable place
// (deref of `&mut`), and `add_assign` autorefs to `&mut Self`. The
// equivalent plain assignment `*p = …;` does work. But compound
// assignment fails because `is_mutable_place` only recognizes
// `Var`-rooted chains (Var/FieldAccess/TupleIndex); it returns false
// for `Deref`. The dispatch then can't reach the autoref-mut level
// and surfaces "no method `add_assign` on `u32`".
//
// Expected: 42.

fn bump(p: &mut u32) {
    *p += 42;
}

fn answer() -> u32 {
    let mut x: u32 = 0;
    bump(&mut x);
    x
}
