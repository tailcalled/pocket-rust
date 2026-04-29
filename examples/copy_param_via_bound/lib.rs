fn dup<T: Copy>(t: T) -> T {
    let s: T = t;
    t
}

fn answer() -> u32 { dup(42) }
