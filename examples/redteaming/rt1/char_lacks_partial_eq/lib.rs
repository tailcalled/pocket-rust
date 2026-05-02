// `char` is a Copy primitive with `as` casts to/from every integer
// kind, but `lib/std/cmp.rs` doesn't declare `impl PartialEq for
// char`. So `'a' == 'b'` errors "no method `eq` on `char`" — a
// stdlib gap. Same shape exists for the other comparison traits
// (Eq/PartialOrd/Ord), and probably for any future trait we add to
// the cmp family.
//
// Expected: 42.

fn answer() -> u32 {
    let a: char = 'a';
    let b: char = 'b';
    if a == b { 0 } else { 42 }
}
