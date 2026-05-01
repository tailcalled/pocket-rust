enum Greeting<'a> {
    Empty,
    Msg(&'a str),
}

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
    let g: Greeting = Greeting::Msg(hello());
    match g {
        Greeting::Msg(s) => (s.len() as u32) + 37,
        Greeting::Empty => 0,
    }
}
