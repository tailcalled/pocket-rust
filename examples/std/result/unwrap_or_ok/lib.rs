// `Result::unwrap_or` on `Ok(v)` returns `v`, ignoring the default.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Ok(42);
    r.unwrap_or(0)
}
