// `self.or(res)` on Ok(v) returns Ok(v); res is ignored.
fn answer() -> u32 {
    let a: Result<u32, u32> = Result::Ok(42);
    let b: Result<u32, u32> = Result::Err(7);
    a.or(b).unwrap_or(0)
}
