// `Result<Result<T, E>, E>::flatten` on Ok(Err(e)) → Err(e).
fn answer() -> u32 {
    let r: Result<Result<u32, u32>, u32> = Result::Ok(Result::Err(42));
    match r.flatten() {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    }
}
