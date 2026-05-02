// `Result::is_ok` returns true on an `Ok`-shaped result.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Ok(42);
    if r.is_ok() { 42 } else { 0 }
}
