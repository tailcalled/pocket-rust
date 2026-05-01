fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let p64: *mut u64 = ¤cast::<u64, u8>(p);
        *p64 = 9000000000;
        let back: *mut u8 = ¤cast::<u8, u64>(p64);
        let p64_again: *mut u64 = ¤cast::<u64, u8>(back);
        let v: u64 = *p64_again;
        ¤free(p);
        v as u32
    }
}
