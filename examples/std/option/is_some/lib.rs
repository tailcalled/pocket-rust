// `Option::is_some` returns true on a `Some`-shaped option.
fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42);
    if o.is_some() { 42 } else { 0 }
}
