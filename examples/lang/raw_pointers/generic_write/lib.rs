fn write_through<T>(dst: *mut T, v: T) {
    unsafe { *dst = v; }
}
fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(4);
        let pu: *mut u32 = ¤cast::<u32, u8>(p);
        write_through::<u32>(pu, 42);
        *pu
    }
}
