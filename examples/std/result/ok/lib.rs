// `Result::ok` converts to `Option<T>`, dropping the error.
fn answer() -> u32 {
    let r: Result<u32, u32> = Result::Ok(42);
    let o: Option<u32> = r.ok();
    o.unwrap_or(0)
}
