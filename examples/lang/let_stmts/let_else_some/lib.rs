fn lookup() -> Option<u32> { Option::Some(42) }

fn answer() -> u32 {
    let Option::Some(x) = lookup() else { return 0; };
    x
}
