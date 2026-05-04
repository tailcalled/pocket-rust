// Derive on a unit / zero-field struct.

#[deriving(Copy, Clone, PartialEq, Eq)]
struct Marker {}

fn answer() -> u32 {
    let m: Marker = Marker {};
    let n: Marker = m.clone();
    if m.eq(&n) { 42u32 } else { 0u32 }
}
