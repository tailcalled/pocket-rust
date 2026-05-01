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

fn world() -> &'static str {
    let p: *mut u8 = unsafe { ¤alloc(3) };
    unsafe {
        *p = 119;
        *(p.byte_add(1)) = 111;
        *(p.byte_add(2)) = 119;
        ¤make_str(p as *const u8, 3)
    }
}

fn pick<'a>(b: bool, a: &'a str, c: &'a str) -> &'a str {
    if b { a } else { c }
}

fn answer() -> u32 {
    let s = pick(false, hello(), world());
    (s.len() as u32) + 39  // 3 + 39
}
