// `*const T::byte_add(self, count)` advances the pointer by `count`
// bytes. Same shape as the underlying `¤ptr_usize_add` intrinsic.
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let p0: *mut u32 = ¤cast::<u32, u8>(p);
        *p0 = 10;
        let p4_u8: *mut u8 = p.byte_add(4);
        let p4: *mut u32 = ¤cast::<u32, u8>(p4_u8);
        *p4 = 32;
        let v: u32 = *p0 + *p4;
        ¤free(p);
        v
    }
}
