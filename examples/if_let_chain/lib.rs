enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let o: Option<Option<u32>> = Option::Some(Option::Some(42));
    if let Option::Some(inner) = o {
        if let Option::Some(n) = inner {
            n
        } else {
            0
        }
    } else {
        1
    }
}
