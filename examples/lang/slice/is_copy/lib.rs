// `&[T]` is Copy (like `&T`), so we can pass the same fat ref to
// two consumers without borrowck rejecting the second use.
fn count(s: &[u32]) -> u32 {
    s.len() as u32
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    let s = v.as_slice();
    let a: u32 = count(s);
    let b: u32 = count(s);  // second use — only legal if `s` is Copy.
    (a + b) * 4 + 2
}
