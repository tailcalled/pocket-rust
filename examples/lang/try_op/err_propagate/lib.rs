// `?` on Err short-circuits, returning Err early from the enclosing
// function. The surrounding code never executes.
fn fetch(b: bool) -> Result<u32, u32> {
    if b {
        Result::Ok(7)
    } else {
        Result::Err(42)
    }
}

fn doit() -> Result<u32, u32> {
    let _v = fetch(false)?;
    // Unreached: fetch returned Err, so `?` propagated.
    Result::Ok(0)
}

fn answer() -> u32 {
    match doit() {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    }
}
