// Mutable borrow of a destructured binding works independently of
// other elements. `(mut a, mut b)` from a u32 pair: take `&mut a`,
// write through it; take `&mut b` after a's borrow ends, write
// through it; sum the two reads.
fn answer() -> u32 {
    let pair: (u32, u32) = (10u32, 30u32);
    let (mut a, mut b) = pair;
    {
        let r = &mut a;
        *r = *r + 1u32;
    }
    {
        let s = &mut b;
        *s = *s + 1u32;
    }
    a + b
}
