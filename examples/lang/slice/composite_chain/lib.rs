// Composite: build a struct containing a slice, wrap in Option,
// return from a function (sret), match arm extracts struct, struct
// field read pulls the slice back out, method call on that slice
// reads its length. Tests the slice through several ABI boundaries
// in a single dataflow chain — fat ref must arrive intact at each
// step.
struct View<'a> { tag: u32, data: &'a [u32] }

fn maybe_view<'a>(s: &'a [u32]) -> Option<View<'a>> {
    if s.is_empty() {
        Option::None
    } else {
        Option::Some(View { tag: 7, data: s })
    }
}

fn count(v: &View) -> u32 {
    v.tag + (v.data.len() as u32)
}

fn answer() -> u32 {
    let mut buf: Vec<u32> = Vec::new();
    buf.push(1);
    buf.push(2);
    buf.push(3);
    buf.push(4);  // len = 4
    let total: u32 = match maybe_view(buf.as_slice()) {
        Option::Some(v) => count(&v),  // 7 + 4 = 11
        Option::None => 0,
    };
    total + 31  // 11 + 31 = 42
}
