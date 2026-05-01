// `Vec<&[u32]>` — Vec whose elements are themselves fat refs (8
// bytes each). The buffer is `cap * 8` bytes; each `push(slice)`
// stores both halves at the matching offset.
fn answer() -> u32 {
    let mut a: Vec<u32> = Vec::new();
    a.push(1);
    a.push(2);
    a.push(3);  // a.len() = 3
    let mut b: Vec<u32> = Vec::new();
    b.push(10);
    b.push(20);  // b.len() = 2

    let mut v: Vec<&[u32]> = Vec::new();
    v.push(a.as_slice());
    v.push(b.as_slice());
    let popped = match v.pop() {
        Option::Some(s) => s.len() as u32,  // = 2
        Option::None => 0,
    };
    let popped2 = match v.pop() {
        Option::Some(s) => s.len() as u32,  // = 3
        Option::None => 0,
    };
    // 2 + 3 = 5; 42 - 5 = 37, so add 37.
    popped + popped2 + 37
}
