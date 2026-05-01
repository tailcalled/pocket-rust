// Pass `&str` as a function arg; observe its length on the other
// side. Same fat-ref ABI as `&[u8]` — 2 i32s in the wasm signature.
fn count(s: &str) -> u32 {
    s.len() as u32
}

// Helper to build a 5-byte `&str` ("hello") via the unsafe raw-parts
// route. String literals (the canonical construction path) are a
// later milestone; for now `¤make_str` is what we have. The result's
// lifetime is `'static` because the heap allocation is never freed.
fn five_bytes() -> &'static str {
    let p: *mut u8 = unsafe { ¤alloc(5) };
    unsafe {
        *p = 104;                       // 'h'
        *(p.byte_add(1)) = 101;         // 'e'
        *(p.byte_add(2)) = 108;         // 'l'
        *(p.byte_add(3)) = 108;         // 'l'
        *(p.byte_add(4)) = 111;         // 'o'
        ¤make_str(p as *const u8, 5)
    }
}

fn answer() -> u32 {
    count(five_bytes()) + 37
}
