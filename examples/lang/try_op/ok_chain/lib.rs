// `?` on Ok extracts the value and continues.
fn fetch(b: bool) -> Result<u32, u32> {
    if b {
        Result::Ok(42)
    } else {
        Result::Err(7)
    }
}

fn doit() -> Result<u32, u32> {
    let v = fetch(true)?;
    Result::Ok(v)
}

fn answer() -> u32 {
    match doit() {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    }
}
