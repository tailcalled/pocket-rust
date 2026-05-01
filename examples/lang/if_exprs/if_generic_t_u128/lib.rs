fn pick<T>(b: bool, x: T, y: T) -> T {
    if b { x } else { y }
}

fn answer() -> u128 {
    pick::<u128>(true, 42, 99)
}
