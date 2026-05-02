// `Result<Option<T>, E>::transpose`: Ok(Some(v)) → Some(Ok(v)).
fn answer() -> u32 {
    let r: Result<Option<u32>, u32> = Result::Ok(Option::Some(42));
    let o: Option<Result<u32, u32>> = r.transpose();
    match o {
        Option::Some(inner) => inner.unwrap_or(0),
        Option::None => 0,
    }
}
