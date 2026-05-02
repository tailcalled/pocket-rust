// Two impls whose method receiver types live at *different* levels
// of the candidate-self-type chain — so dispatch is unambiguous and
// the chain-first match wins.
//
// `r: &u32; r.wow()` chain: [&u32, &&u32, ...].
// - `impl Wow for u32` method recv type = `&u32` (Self=u32, &Self=&u32).
//     Matches level &u32 directly.
// - `impl Wow for &u32` method recv type = `&&u32` (Self=&u32).
//     Would match level &&u32, but we never reach that — chain order
//     picks &u32 first.
// → returns 1. Mirrors rustc.

trait Wow {
    fn wow(&self) -> u32;
}

impl Wow for u32 {
    fn wow(&self) -> u32 { 1 }
}

impl Wow for &u32 {
    fn wow(&self) -> u32 { 2 }
}

fn answer() -> u32 {
    let x: u32 = 5;
    let r: &u32 = &x;
    r.wow()
}
