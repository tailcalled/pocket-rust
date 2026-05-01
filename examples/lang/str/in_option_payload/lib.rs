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

fn maybe<'a>(present: bool, s: &'a str) -> Option<&'a str> {
    if present { Option::Some(s) } else { Option::None }
}

fn answer() -> u32 {
    match maybe(true, hello()) {
        Option::Some(s) => (s.len() as u32) + 37,
        Option::None => 0,
    }
}
