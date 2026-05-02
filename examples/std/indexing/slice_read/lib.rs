// Indexing through `&[T]` — `[T]`'s Index impl is selected.
fn sum(s: &[u32]) -> u32 {
    s[0] + s[1] + s[2]
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(10);
    v.push(20);
    v.push(12);
    sum(v.as_slice())
}
