// `match` taking the `Err` arm and recovering with the error value.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Err(42);
    match r {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    }
}
