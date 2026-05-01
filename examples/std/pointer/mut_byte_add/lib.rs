// `*mut T::byte_add` returns `*mut T` (mutability preserved). Write
// through the offset pointer to confirm the *mut path works end-to-end.
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let p0: *mut u32 = ¤cast::<u32, u8>(p);
        *p0 = 0;
        let p4_u8: *mut u8 = p.byte_add(4);
        let p4: *mut u32 = ¤cast::<u32, u8>(p4_u8);
        *p4 = 42;
        let v: u32 = *p4;
        ¤free(p);
        v
    }
}
