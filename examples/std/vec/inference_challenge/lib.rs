// Stress-test Vec's type inference. Multiple challenges:
//   1. `Vec::new()` with no annotation — T must be inferred from a
//      later `push(...)` site (no annotation on the binding itself).
//   2. `Vec<Option<u32>>` — element is itself an enum; push takes
//      the i32-flattened address but must memcpy the 8 bytes of
//      Option<u32> into the buffer slot.
//   3. A generic free function `first<T>(v: &Vec<T>) -> Option<&T>`
//      that propagates T through the borrow → method-call → return
//      chain, then back into the caller's match arm.
//   4. `Vec<Pt>` where `Pt` has multiple fields — exercises
//      multi-leaf store/load through *mut T.
//   5. A literal arg to push with no surrounding type annotation —
//      the integer literal var must unify with T after T is pinned
//      by an earlier push of a typed binding.
//
// Each piece's expected value gates its contribution to the final
// answer through a per-check `if`: a wrong intermediate zeros its
// piece rather than cancelling against another, so any regression
// surfaces as a numeric mismatch instead of being absorbed.

struct Pt { x: u32, y: u32 }

fn first<T>(v: &Vec<T>) -> Option<&T> {
    v.get(0)
}

fn answer() -> u32 {
    // (1) + (5): no annotation on `v`; T determined by push args.
    let mut v = Vec::new();
    let typed: u32 = 10;
    v.push(typed);  // pins T = u32
    v.push(20);     // literal — must infer u32 from T
    v.push(12);

    // Sum via repeated pop; verifies push order and read-back.
    let mut sum: u32 = 0;
    while v.len() > 0 {
        let n: u32 = match v.pop() {
            Option::Some(x) => x,
            Option::None => 0,
        };
        sum = sum + n;
    }
    // sum = 10 + 20 + 12 = 42

    // (2): Vec<Option<u32>>; element-as-enum forces memcpy on push.
    let mut nested: Vec<Option<u32>> = Vec::new();
    nested.push(Option::Some(7));
    nested.push(Option::None);
    nested.push(Option::Some(35));
    let some_top: u32 = match nested.pop() {
        Option::Some(o) => match o {
            Option::Some(x) => x,
            Option::None => 0,
        },
        Option::None => 0,
    };
    // some_top = 35 (top of stack)

    // (4): Vec<Pt> with multi-field struct.
    let mut points: Vec<Pt> = Vec::new();
    points.push(Pt { x: 100, y: 200 });
    points.push(Pt { x: 300, y: 400 });

    // (3): generic free function over &Vec<Pt>; chain through it.
    let first_x: u32 = match first(&points) {
        Option::Some(p) => p.x,
        Option::None => 0,
    };
    // first_x = 100

    // Per-check gating: each piece contributes its share of 42
    // only if the corresponding intermediate matches its expected
    // value. Any regression in any piece zeros its share, breaking
    // the total. The shares are coprime-ish (12 + 14 + 16 = 42)
    // so swapping or partial corruption can't accidentally re-sum.
    let a: u32 = if sum == 42 { 12 } else { 0 };
    let b: u32 = if some_top == 35 { 14 } else { 0 };
    let c: u32 = if first_x == 100 { 16 } else { 0 };
    a + b + c
}
