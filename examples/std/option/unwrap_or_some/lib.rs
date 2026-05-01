// `unwrap_or` returns the inner `Some` value when the option is some.
fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42);
    o.unwrap_or(0)
}
