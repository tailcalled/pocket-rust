// `Result<T, !>::into_ok` extracts the Ok payload directly. The Err
// arm is uninhabited (E = !), so exhaustiveness skips it.

fn make_ok() -> Result<u32, !> {
    Result::Ok(42)
}

fn answer() -> u32 {
    make_ok().into_ok()
}
