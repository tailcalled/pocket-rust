// `vec![]` (no elements) — the inner `Vec::new()` is generic, so
// the element type comes from the let binding's annotation.

fn answer() -> u32 {
    let v: Vec<u32> = vec![];
    let mut total: u32 = 42;
    if v.is_empty() { total } else { 0 }
}
