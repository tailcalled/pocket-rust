// `Result::is_err` returns true on an `Err`-shaped result.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Err(7);
    if r.is_err() { 42 } else { 0 }
}
