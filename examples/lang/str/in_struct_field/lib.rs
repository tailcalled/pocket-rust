// `&str` as a struct field. 8-byte fat ref in struct layout.
struct Greeting<'a> { msg: &'a str, weight: u32 }

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
    let g: Greeting = Greeting { msg: hello(), weight: 32 };
    (g.msg.len() as u32) + g.weight + 5
}
