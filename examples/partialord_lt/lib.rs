fn smaller<T: PartialOrd>(a: T, b: T) -> bool {
    a.lt(&b)
}

fn answer() -> u32 {
    let a: u32 = 3;
    let b: u32 = 5;
    if smaller(a, b) { 42 } else { 0 }
}
