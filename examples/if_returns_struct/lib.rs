struct Pair { a: u32, b: u64 }

fn make(b: bool) -> Pair {
    if b {
        Pair { a: 7, b: 9000000000 }
    } else {
        Pair { a: 1, b: 2 }
    }
}

fn answer() -> u64 {
    let p: Pair = make(true);
    p.b
}
