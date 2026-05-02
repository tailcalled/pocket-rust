// `Result<Option<T>, E>::transpose`: Ok(None) → None.
fn answer() -> u32 {
    let r: Result<Option<u32>, u32> = Result::Ok(Option::None);
    let o: Option<Result<u32, u32>> = r.transpose();
    match o {
        Option::Some(_) => 0,
        Option::None => 42,
    }
}
