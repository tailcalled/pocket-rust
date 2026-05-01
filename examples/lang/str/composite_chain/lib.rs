struct View<'a> { tag: u32, msg: &'a str }

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

fn maybe_view<'a>(s: &'a str) -> Option<View<'a>> {
    if s.is_empty() {
        Option::None
    } else {
        Option::Some(View { tag: 7, msg: s })
    }
}

fn count(v: &View) -> u32 {
    v.tag + (v.msg.len() as u32)
}

fn answer() -> u32 {
    let total: u32 = match maybe_view(hello()) {
        Option::Some(v) => count(&v),  // 7 + 5 = 12
        Option::None => 0,
    };
    total + 30  // 42
}
