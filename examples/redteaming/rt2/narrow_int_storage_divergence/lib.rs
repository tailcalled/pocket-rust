// The same `u8` value reads back differently depending on whether
// it spent any time in linear memory.
//
// In a wasm-local, a `u8` is just a 32-bit i32 with no enforced
// invariant. Arithmetic on it produces full-width results
// (see the rt2_narrow_int_add_no_wrap sibling example).
//
// In linear memory, a `u8` field is stored via `I32Store8` (which
// silently truncates the wasm i32 to its low byte) and read back via
// `I32Load8U` (which zero-extends the byte). So *the act of round-
// tripping through memory* enforces the 8-bit invariant.
//
// Result: `let a: u8 = 255u8 + 43u8;` gives `a as u32 == 298` (no
// memory round-trip), while `let b: u8 = 255u8 + 43u8; let r: &u8 =
// &b; let m = *r;` gives `m as u32 == 42` (round-trip via `&b`
// forces `b` into Memory storage). Two semantically identical
// operations producing different observable values, gated only on
// the layout pass's escape-analysis decision.
//
// Why architectural: the storage-decision rule (`Storage::Local` vs
// `Storage::Memory{...}`) was designed as a representation choice
// invisible to the program. With the narrow-int representation gap,
// it leaks: the value the program sees depends on whether `&` ever
// touched the binding. This is an observable break of layout
// independence — exactly the kind of "spooky action at a distance"
// that the original `compute_layout` was supposed to avoid.
//
// Expected: 42 (post-fix, both paths give 42 — the masked u8 wraps
// to 42 in both local and memory storage).

fn answer() -> u32 {
    let a: u8 = 255u8 + 43u8;
    let b: u8 = 255u8 + 43u8;
    let r: &u8 = &b;
    let m: u8 = *r;
    if a as u32 == m as u32 { 42u32 } else { 0u32 }
}
