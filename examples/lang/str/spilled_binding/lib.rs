fn deref_count(p: &&str) -> u32 { (*p).len() as u32 }

fn hello() -> &'static str {
    let p: *mut u8 = unsafe { ¤alloc(5) };
    unsafe {
        *p = 104;
        *(p.byte_add(1)) = 101;
        *(p.byte_add(2)) = 108;
        *(p.byte_add(3)) = 108;
        *(p.byte_add(4)) = 111;
        ¤make_str(p as *const u8, 5)
    }
}

fn answer() -> u32 {
    let s: &str = hello();
    let n = deref_count(&s);  // forces spill of `s`
    let m = s.len() as u32;   // read after spill — must agree
    (n + m) * 4 + 2  // 10 * 4 + 2 = 42
}
