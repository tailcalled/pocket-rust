fn s_of_len(n: usize) -> &'static str {
    let p: *mut u8 = unsafe { ¤alloc(n) };
    let mut i: usize = 0;
    while i < n {
        unsafe { *(p.byte_add(i)) = 65; }  // 'A'
        i = i + 1;
    }
    unsafe { ¤make_str(p as *const u8, n) }
}

fn answer() -> u32 {
    let mut v: Vec<&str> = Vec::new();
    v.push(s_of_len(3));
    v.push(s_of_len(2));
    v.push(s_of_len(7));
    let mut total: u32 = 0;
    while v.len() > 0 {
        let n: u32 = match v.pop() {
            Option::Some(s) => s.len() as u32,
            Option::None => 0,
        };
        total = total + n;
    }
    total + 30  // 3+2+7=12; 12+30=42
}
