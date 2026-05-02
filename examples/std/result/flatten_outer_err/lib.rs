// `Result<Result<T, E>, E>::flatten` on Err(e) → Err(e).
fn answer() -> u32 {
    let r: Result<Result<u32, u32>, u32> = Result::Err(42);
    match r.flatten() {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    }
}
