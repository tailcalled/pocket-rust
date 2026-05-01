enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42);
    let r: &Option<u32> = &o;
    match r {
        &Option::Some(n) => n,
        &Option::None => 0,
    }
}
