// `*const T as usize` exposes the pointer's address as an integer.
// Test the *spacing* between two consecutive 4-byte allocations: the
// bump allocator should advance by exactly 4 between them. Earlier
// versions of this test pinned the absolute address ("first alloc
// lands at offset 8"), which is brittle — any new string literal
// baked into the data segment bumps `__heap_top` past 8 and breaks
// the test for unrelated stdlib changes.
fn answer() -> u32 {
    unsafe {
        let p1: *mut u8 = ¤alloc(4);
        let p2: *mut u8 = ¤alloc(4);
        let a1: usize = p1 as usize;
        let a2: usize = p2 as usize;
        ¤free(p1);
        ¤free(p2);
        (a2 - a1) as u32
    }
}
