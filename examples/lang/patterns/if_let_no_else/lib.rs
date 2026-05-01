enum Option<T> {
    Some(T),
    None,
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(7);
    let mut n: u32 = 0;
    if let Option::Some(x) = o {
        n = x + 35;
    }
    n
}
