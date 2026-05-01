fn pick<T>(b: bool, x: T, y: T) -> T {
    if b { x } else { y }
}

fn answer() -> u32 {
    pick::<u32>(true, 42, 99)
}
