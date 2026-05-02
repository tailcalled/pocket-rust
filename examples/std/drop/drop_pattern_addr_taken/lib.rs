// Borrowing through pattern bindings: tuple destructure produces
// independent owned bindings that must be borrowable in parallel.
// `&_a` and `&_b` are taken simultaneously; both reads through the
// references return the original element values.
fn answer() -> u32 {
    let pair = (10u32, 32u32);
    let (_a, _b) = pair;
    let r = &_a;
    let s = &_b;
    *r + *s
}
