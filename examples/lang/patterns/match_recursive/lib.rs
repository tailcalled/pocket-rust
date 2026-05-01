enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let x: Option<Option<u32>> = Option::Some(Option::Some(42));
    match x {
        Option::Some(Option::Some(n)) => n,
        Option::Some(Option::None) => 0,
        Option::None => 1,
    }
}
