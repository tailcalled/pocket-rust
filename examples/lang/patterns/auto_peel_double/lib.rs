// Match ergonomics: two layers of `&` peel through to the variant.
// Inside the pattern, x ends up as `&u32` (the outer `&mut` got
// demoted to `&` once we peeled past the inner `&`).

fn pick(o: &&Option<u32>) -> u32 {
    match o {
        Option::Some(x) => *x,
        Option::None => 0u32,
    }
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42u32);
    let r: &Option<u32> = &o;
    pick(&r)
}
