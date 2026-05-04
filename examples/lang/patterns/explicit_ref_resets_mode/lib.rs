// `&pat` resets the binding mode to Move — the user is explicitly
// stripping the ref themselves. So `&Option::Some(x)` against
// `&Option<u32>` makes x a by-value u32 (Copy here, so no move
// rejection).

fn pick(o: &Option<u32>) -> u32 {
    match o {
        &Option::Some(x) => x,
        &Option::None => 0u32,
    }
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42u32);
    pick(&o)
}
