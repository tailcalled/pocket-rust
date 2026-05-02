fn lookup() -> Option<u32> { Option::None }

fn answer() -> u32 {
    let Option::Some(_x) = lookup() else { return 42; };
    0
}
