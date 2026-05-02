// Compound assignment through a raw pointer inside an `unsafe` block.
// `*p` is a place (deref of `*mut u32`), but `is_mutable_place`
// doesn't recognize Deref, so the autoref-mut dispatch level for
// `add_assign` is skipped and the call surfaces "no method
// `add_assign` on `u32`" — a misleading error that buries the real
// dispatch issue (and never gets a chance to surface a safeck-style
// rejection if the user had forgotten `unsafe`).
//
// Expected: 42 (after fixing problem 3, the dispatch should succeed
// inside the unsafe block).

fn answer() -> u32 {
    let mut x: u32 = 0;
    let p: *mut u32 = &mut x as *mut u32;
    unsafe {
        *p += 42;
    }
    x
}
