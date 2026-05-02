// `Result<Option<T>, E>::transpose`: Err(e) → Some(Err(e)).
fn answer() -> u32 {
    let r: Result<Option<u32>, u32> = Result::Err(42);
    let o: Option<Result<u32, u32>> = r.transpose();
    match o {
        Option::Some(inner) => match inner {
            Result::Ok(_) => 0,
            Result::Err(e) => e,
        },
        Option::None => 0,
    }
}
