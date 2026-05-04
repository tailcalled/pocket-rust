// PartialOrd derive on an enum: variants compare by declaration order
// (V0 < V1 < V2). Same-variant values recurse lexicographically through
// the payload — matches Rust's derive semantics.

#[deriving(PartialEq, PartialOrd)]
enum E {
    A,
    B(u32),
    C { x: u32 },
}

fn answer() -> u32 {
    let a: E = E::A;
    let b1: E = E::B(1u32);
    let b9: E = E::B(9u32);
    let c: E = E::C { x: 5u32 };
    // a < b1 (variant order), b1 < b9 (payload order), b9 < c (variant order).
    let chain_ok: bool = a.lt(&b1) && b1.lt(&b9) && b9.lt(&c);
    // strict ordering: a is not less than itself.
    let strict_ok: bool = ¤bool_not(a.lt(&a));
    // gt symmetry.
    let gt_ok: bool = c.gt(&a) && b9.gt(&b1);
    if chain_ok && strict_ok && gt_ok { 42u32 } else { 0u32 }
}
