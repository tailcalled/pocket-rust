// `*const T as usize` exposes the pointer's address as an integer.
// Heap allocations bump from offset 8 (the `__heap_top` initial value);
// the first 4-byte `¤alloc` should land at exactly 8.
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(4);
        let addr: usize = p as usize;
        ¤free(p);
        addr as u32
    }
}
