struct Wrap<'a> { a: u32, msg: &'a str, b: u32 }

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

fn make<'a>(s: &'a str) -> Wrap<'a> {
    Wrap { a: 30, msg: s, b: 7 }
}

fn answer() -> u32 {
    let w = make(hello());
    w.a + (w.msg.len() as u32) + w.b  // 30 + 5 + 7
}
