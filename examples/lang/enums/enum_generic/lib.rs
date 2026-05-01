enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let _x: Option<u32> = Option::Some(42);
    let _y: Option<u32> = Option::None;
    42
}
