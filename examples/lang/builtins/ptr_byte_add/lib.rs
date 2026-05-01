// `¤ptr_usize_add(p, n)` advances the pointer by n bytes. Allocate
// 8 bytes, write a u32 at offset 0 and another at offset 4 (using the
// raw intrinsic), read them back, return the sum.
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let p0: *mut u32 = ¤cast::<u32, u8>(p);
        *p0 = 30;
        let p4_as_u8: *mut u8 = ¤ptr_usize_add(p, 4);
        let p4: *mut u32 = ¤cast::<u32, u8>(p4_as_u8);
        *p4 = 12;
        let v: u32 = *p0 + *p4;
        ¤free(p);
        v
    }
}
