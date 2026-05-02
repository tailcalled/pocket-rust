// `Result::unwrap_or` on `Err(_)` returns the supplied default.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Err(7);
    r.unwrap_or(42)
}
