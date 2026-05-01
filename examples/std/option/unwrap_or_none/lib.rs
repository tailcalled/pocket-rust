// `unwrap_or` falls back to the supplied default when the option is none.
fn answer() -> u32 {
    let o: Option<u32> = Option::None;
    o.unwrap_or(42)
}
