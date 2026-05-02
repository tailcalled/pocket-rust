// `Result<!, E>::into_err` extracts the Err payload directly. The Ok
// arm is uninhabited (T = !), so exhaustiveness skips it.

fn make_err() -> Result<!, u32> {
    Result::Err(42)
}

fn answer() -> u32 {
    make_err().into_err()
}
