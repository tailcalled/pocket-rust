// Direct `match` on a `Result` with both arms covered.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Ok(42);
    match r {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    }
}
