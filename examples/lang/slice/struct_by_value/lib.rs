// Return a struct that contains a slice field by value. The whole
// struct flattens to 4 i32s (3 for u32 fields + 2 for the fat ref —
// wait, actually: Wrap { a: u32, s: &[u32], b: u32 } flattens to
// [I32, I32, I32, I32] = 4 leaves. multi-value return ABI must carry
// all four out.
struct Wrap<'a> { a: u32, s: &'a [u32], b: u32 }

fn make<'a>(s: &'a [u32]) -> Wrap<'a> {
    Wrap { a: 30, s, b: 8 }
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    let w = make(v.as_slice());
    w.a + (w.s.len() as u32) + w.b
}
