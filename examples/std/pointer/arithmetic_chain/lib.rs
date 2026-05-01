// Chain `byte_add` / `byte_sub` / `byte_offset` and confirm the
// final read lands on the value written through the original pointer.
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(16);
        let p0: *mut u32 = ¤cast::<u32, u8>(p);
        *p0 = 42;
        let q: *mut u8 = p.byte_add(8).byte_sub(4).byte_offset(-4);
        let q32: *mut u32 = ¤cast::<u32, u8>(q);
        let v: u32 = *q32;
        ¤free(p);
        v
    }
}
