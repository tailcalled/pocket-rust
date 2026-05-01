// `Option::is_none` returns true on a `None`-shaped option.
fn answer() -> u32 {
    let o: Option<u32> = Option::None;
    if o.is_none() { 42 } else { 0 }
}
