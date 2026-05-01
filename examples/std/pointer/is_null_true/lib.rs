// A null pointer (cast from 0) is_null → true.
fn answer() -> u32 {
    let p: *const u32 = 0 as *const u32;
    if p.is_null() { 42 } else { 0 }
}
