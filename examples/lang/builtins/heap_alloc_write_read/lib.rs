fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(4);
        let q: *mut u32 = ¤cast::<u32, u8>(p);
        *q = 42;
        let v: u32 = *q;
        ¤free(p);
        v
    }
}
