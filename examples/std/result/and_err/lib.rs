// `self.and(res)` on Err(e) returns Err(e); res is ignored.
fn answer() -> u32 {
    let a: Result<u32, u32> = Result::Err(42);
    let b: Result<u32, u32> = Result::Ok(7);
    let r: Result<u32, u32> = a.and(b);
    match r {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    }
}
