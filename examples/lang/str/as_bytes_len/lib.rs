// `str::as_bytes` returns `&[u8]` whose len matches the str's len.
// Tests the fat-ref pass-through between &str and &[u8].
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
    let bytes: &[u8] = s.as_bytes();
    (s.len() as u32) + (bytes.len() as u32) + 32  // 5 + 5 + 32
}
