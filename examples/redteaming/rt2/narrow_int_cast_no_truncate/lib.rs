// `as` casts within the Narrow32 integer class don't truncate.
//
// `emit_int_to_int_cast` (src/codegen.rs:3021) emits a wasm op only
// when source and target classes differ (Narrow32 / Wide64 / Wide128).
// All eight Narrow32 kinds (u8/i8/u16/i16/u32/i32/usize/isize) share
// the same class, so casting *between any of them* emits zero
// instructions. The same i32 wasm value passes through unchanged.
//
// `298u32 as u8` therefore stays 298 instead of becoming 42; `(-214i32)
// as u8` stays the i32 representation of -214 instead of becoming 42
// (the two-byte truncation of -214 mod 256). The bug compounds with
// the narrow-int-arithmetic gap: any code path that produces a >255
// "u8" via arithmetic and then casts it to u32 (or stores it to
// another u8 variable) carries the wrong value.
//
// Why architectural: same root cause as the arithmetic gap —
// Narrow32 representation is implicit. The cast layer assumes the
// invariant ("a `u8` value is already masked to 8 bits"); the
// arithmetic layer assumes the cast layer will fix it up. Neither
// does. The fix has to add an explicit mask (`I32Const 0xFF`,
// `I32And`) for unsigned narrow targets and a sign-extend chain
// (`I32Const 24`, `I32Shl`, `I32Const 24`, `I32ShrS`) for signed
// narrow targets — for every Narrow32→Narrow32 cast where the
// target is narrower than the source.
//
// Expected: 42 (post-fix, `298u32 as u8` truncates to 42).

fn answer() -> u32 {
    let big: u32 = 298u32;
    let small: u8 = big as u8;
    small as u32
}
