// The function body returns `&'a u32` as `&'b u32` — sound only
// when `'a: 'b`. Real Rust requires the user to write the
// predicate; without it, the function's body is rejected at
// borrowck because the lifetime relation isn't established.
//
// Pocket-rust silently accepts: lifetime relations aren't
// solved at all today. Adding `where 'a: 'b` (rt4#6) made the
// declared form parse and validate, but borrowck still doesn't
// consume the predicate to allow what it'd otherwise reject —
// nor does it reject when the predicate is missing.
//
// Expected post-fix: this fn errors WITHOUT the predicate; with
// `where 'a: 'b` added it compiles. Confirms that
// `lifetime_predicates` actually shapes borrowck's reasoning
// rather than being dead storage.

fn shorten<'a, 'b>(x: &'a u32, _y: &'b u32) -> &'b u32 {
    // `&'a` standing in as `&'b` requires 'a: 'b; absent the
    // predicate, real Rust rejects.
    x
}

pub fn answer() -> u32 {
    let a: u32 = 21u32;
    let b: u32 = 21u32;
    *shorten(&a, &b) + 21u32
}
