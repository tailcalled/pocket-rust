// `&[T]` as a struct field. Layout: 8 bytes per fat ref. The struct
// `Wrap<'a> { s: &'a [u32], pad: u32 }` totals 12 bytes if the
// codegen lays out fields sequentially. We construct with a Vec-
// backed slice, store, then read `s.len()` back through the field.
struct Wrap<'a> { s: &'a [u32], pad: u32 }

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    v.push(0);
    let w: Wrap = Wrap { s: v.as_slice(), pad: 36 };
    w.s.len() as u32 + w.pad
}
