fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let p0: *mut u32 = ¤cast::<u32, u8>(p);
        *p0 = 42;
        let p_plus: *mut u8 = p.byte_add(4);
        let p_back: *mut u8 = p_plus.byte_sub(4);
        let p_back_u32: *mut u32 = ¤cast::<u32, u8>(p_back);
        let v: u32 = *p_back_u32;
        ¤free(p);
        v
    }
}
