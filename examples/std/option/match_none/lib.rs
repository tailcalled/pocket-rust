fn answer() -> u32 {
    let o: Option<u32> = Option::None;
    match o {
        Option::Some(v) => v,
        Option::None => 42,
    }
}
