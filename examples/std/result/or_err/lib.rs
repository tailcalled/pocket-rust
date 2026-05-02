// `self.or(res)` on Err(_) returns res.
fn answer() -> u32 {
    let a: Result<u32, u32> = Result::Err(7);
    let b: Result<u32, u32> = Result::Ok(42);
    a.or(b).unwrap_or(0)
}
