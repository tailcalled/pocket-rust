// `¤ptr_isize_offset(p, n)` accepts negative n. Walk forward by 4
// then back by -4 to confirm the signed-offset path lands on the
// original pointer.
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let p0: *mut u32 = ¤cast::<u32, u8>(p);
        *p0 = 99;
        let p_plus: *mut u8 = ¤ptr_isize_offset(p, 4);
        let p_back: *mut u8 = ¤ptr_isize_offset(p_plus, -4);
        let p_back_u32: *mut u32 = ¤cast::<u32, u8>(p_back);
        let v: u32 = *p_back_u32;
        ¤free(p);
        v
    }
}
