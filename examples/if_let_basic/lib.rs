enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42);
    if let Option::Some(n) = o {
        n
    } else {
        0
    }
}
