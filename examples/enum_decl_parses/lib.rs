enum Choice {
    A,
    B(u32, u32),
    C { f: u32, g: u32 },
}

enum Option<T> {
    Some(T),
    None,
}

pub enum Pub {
    A,
    B(u32),
}

fn answer() -> u32 {
    42
}
