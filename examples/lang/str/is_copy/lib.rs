fn count(s: &str) -> u32 { s.len() as u32 }

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
    let s = hello();
    let a = count(s);
    let b = count(s);  // second use — only legal if `s` is Copy
    (a + b) * 4 + 2  // 5+5 = 10; 10*4+2 = 42
}
