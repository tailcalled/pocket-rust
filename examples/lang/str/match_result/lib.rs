enum Side { Left, Right }

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

fn pick<'a>(side: Side, a: &'a str, b: &'a str) -> &'a str {
    match side {
        Side::Left => a,
        Side::Right => b,
    }
}

fn answer() -> u32 {
    let empty: &'static str = unsafe { ¤make_str(0 as *const u8, 0) };
    let s = pick(Side::Left, hello(), empty);
    (s.len() as u32) + 37
}
