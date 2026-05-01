// Two `"hello"` literals should share a slot (interning by payload).
// The data segment is built by codegen, so deduping is observable
// only as fewer bytes in the segment — we don't have a way to see
// that directly here, but at minimum both reads must agree on the
// length and content.
fn count(s: &str) -> u32 { s.len() as u32 }

fn answer() -> u32 {
    let a: &str = "hello";
    let b: &str = "hello";
    count(a) + count(b) + 32  // 5 + 5 + 32
}
