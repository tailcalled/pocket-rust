// `vec![a, b, c]` desugars at parse time to a block expression that
// allocates a fresh `Vec` and pushes each element. Element type is
// inferred from the contents (or from surrounding context for the
// empty form).

fn answer() -> u32 {
    let v: Vec<u32> = vec![10, 12, 20];
    let mut sum: u32 = 0;
    let mut i: usize = 0;
    while i < v.len() {
        sum += *v.get(i).unwrap_or(&0);
        i += 1;
    }
    sum
}
