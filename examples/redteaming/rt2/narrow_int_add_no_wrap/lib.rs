// Narrow-integer arithmetic doesn't mask to the type's bit width.
//
// pocket-rust represents every integer narrower than 32 bits (u8/i8/
// u16/i16) as a wasm i32 in wasm-locals. Arithmetic on those locals
// emits raw wasm `I32Add` / `I32Sub` / `I32Mul` instructions, which
// produce a full 32-bit result. The intermediate value is never
// masked back down to 8 (or 16) bits, so a `u8 + u8` whose
// mathematical result exceeds 255 is preserved at full width and
// observed as a > 255 "u8" by every subsequent operation.
//
// Real Rust (release): `255u8 + 43u8 = 42` (wraps modulo 256).
// Real Rust (debug): panics with overflow.
// pocket-rust today: 298 (no wrap, no panic).
//
// Why architectural: pocket-rust's narrow-int representation is
// implicit ("just an i32"). Width is only enforced at memory store
// boundaries (`I32Store8` truncates), not at arithmetic boundaries.
// Anywhere a narrow value crosses a `+`/`-`/`*`/`<<` operator and
// stays in a wasm-local, the type-system invariant `u8 ∈ 0..=256`
// is silently broken. The fix has to mask after every narrow-int
// op (and after every cast that narrows the class — see the cast
// gap in the sister example), so it touches every codegen path
// that emits arithmetic for a narrow IntKind.
//
// Expected: 42 (post-fix, the `as u32` of a wrapped `u8` stays 42).

fn answer() -> u32 {
    let x: u8 = 255u8 + 43u8;
    x as u32
}
