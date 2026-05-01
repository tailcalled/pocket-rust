enum Option<T> {
    Some(T),
    None,
}

fn deref_some(o: &Option<u32>) -> u32 {
    match o {
        &Option::Some(ref x) => *x,
        &Option::None => 0,
    }
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42);
    deref_some(&o)
}
