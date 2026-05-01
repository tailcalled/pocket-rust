fn check<T: Eq>(a: T, b: T) -> bool {
    a.eq(&b)
}

fn answer() -> u32 {
    let a: u32 = 7;
    let b: u32 = 7;
    if check(a, b) { 42 } else { 0 }
}
