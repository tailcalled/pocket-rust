// Multiple `?` calls in sequence — the second only runs if the first
// succeeded.
fn add_one(x: u32) -> Result<u32, u32> {
    if x < 100 {
        Result::Ok(x + 1)
    } else {
        Result::Err(99)
    }
}

fn doit() -> Result<u32, u32> {
    let a = add_one(40)?;
    let b = add_one(a)?;
    Result::Ok(b)
}

fn answer() -> u32 {
    match doit() {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    }
}
