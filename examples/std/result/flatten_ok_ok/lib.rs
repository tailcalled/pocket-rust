// `Result<Result<T, E>, E>::flatten` collapses Ok(Ok(x)) → Ok(x).
fn answer() -> u32 {
    let r: Result<Result<u32, u32>, u32> = Result::Ok(Result::Ok(42));
    r.flatten().unwrap_or(0)
}
