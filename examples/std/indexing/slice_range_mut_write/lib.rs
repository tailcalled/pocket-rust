// `&mut s[..]` writeable — IndexMut<RangeFull> on a `&mut [T]` returns
// the same slice mutably, and a per-element write through the
// re-slice persists in the underlying Vec. Verify by writing 7 to
// `s[2]` via the re-sliced view, then reading back from the Vec.
fn answer() -> u32 {
    let mut v: Vec<u32> = vec![10u32, 20u32, 30u32, 40u32];
    {
        let s: &mut [u32] = v.as_mut_slice();
        let all: &mut [u32] = &mut s[..];
        all[2] = 7u32;
    }
    v[2]
}
