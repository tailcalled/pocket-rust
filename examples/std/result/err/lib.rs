// `Result::err` converts to `Option<E>`, dropping the value.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Err(42);
    let o: Option<u32> = r.err();
    o.unwrap_or(0)
}
