struct Owned {
    n: u32,
}

enum Wrap {
    Some(Owned),
    None,
}

fn unwrap_or_default(w: Wrap) -> Owned {
    match w {
        Wrap::Some(inner) => inner,
        Wrap::None => Owned { n: 0 },
    }
}

fn answer() -> u32 {
    let w: Wrap = Wrap::Some(Owned { n: 42 });
    let o: Owned = unwrap_or_default(w);
    o.n
}
