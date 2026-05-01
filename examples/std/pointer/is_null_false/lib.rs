// A pointer to a stack-allocated value is non-null.
fn answer() -> u32 {
    let x: u32 = 7;
    let p: *const u32 = &x as *const u32;
    if p.is_null() { 0 } else { 42 }
}
