// Return `&str` from a function — multi-value (2-i32) return ABI.
fn forward<'a>(s: &'a str) -> &'a str { s }

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
    let s: &str = forward(hello());
    (s.len() as u32) + 37
}
