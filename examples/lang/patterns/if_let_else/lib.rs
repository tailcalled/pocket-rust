enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let o: Option<u32> = Option::None;
    if let Option::Some(n) = o {
        n
    } else {
        42
    }
}
