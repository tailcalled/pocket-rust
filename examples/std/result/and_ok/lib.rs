// `self.and(res)` on Ok(_) discards self's value and returns res.
fn answer() -> u32 {
    let a: Result<u32, u32> = Result::Ok(1);
    let b: Result<u32, u32> = Result::Ok(42);
    a.and(b).unwrap_or(0)
}
