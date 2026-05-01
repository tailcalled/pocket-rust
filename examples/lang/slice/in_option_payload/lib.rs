// `Option<&[T]>` — Option carries the fat ref as its payload (8
// bytes), so total Option size = 12 bytes (4 disc + 8 payload).
// Functions returning `Option<&[T]>` use sret with that 12-byte
// destination slot. The slice arrives intact on the other side.
fn maybe<'a>(present: bool, s: &'a [u32]) -> Option<&'a [u32]> {
    if present { Option::Some(s) } else { Option::None }
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(10);
    v.push(20);
    v.push(30);
    v.push(40);
    v.push(50);
    let n: u32 = match maybe(true, v.as_slice()) {
        Option::Some(s) => s.len() as u32,
        Option::None => 99,
    };
    n + 37
}
