---
name: types-and-layout
description: Use when working with type representation, primitive types (int kinds, bool, char, str, slices), structs, tuples, enums (tagged-union layout), the never type, or `byte_size_of`/`flatten_rtype` rules. Covers in-memory layout, WASM scalar flattening, and the sret return convention for enums.
---

# types and layout

## Type universe

- Integers: `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `u64`, `i64`, `u128`, `i128`, `usize`, `isize`.
- `bool`.
- `char` — Unicode scalar value 0..=0x10FFFF excluding surrogates 0xD800..=0xDFFF; 4 bytes; flattens to one `i32`; Copy; `as` casts allowed both directions to/from any integer kind.
- Structs.
- Tuples (including the zero-tuple unit type `()`).
- `&T`, `&mut T`, `*const T`, `*mut T`.
- `[T]` (slice DST — only valid behind a reference), `&[T]`, `&mut [T]`.
- `str` (UTF-8 string DST), `&str`, `&mut str`.
- `!` (the never type — has no inhabitants; produced by `break`/`continue`/`return` and by calls to functions returning `!`; coerces freely to any other type at unification time so a diverging arm of `if`/`match` doesn't constrain the construct's type; flattens to no wasm scalars and `byte_size_of` is 0).

## WASM flat layout

`flatten_rtype(rtype, structs, out)` produces the wasm scalar shape:
- ≤32-bit integers (and usize/isize, since we target wasm32) → 1 `i32`.
- 64-bit integers → 1 `i64`.
- 128-bit integers → 2 `i64`s (low half then high half).
- `bool` → 1 `i32`.
- `char` → 1 `i32`.
- Structs → declaration order recursive flatten — `Point { x: u32, y: u64 }` is `[i32, i64]`.
- Tuples → concatenation of element flat scalars. `()` flattens to nothing (so unit-returning functions have an empty result type and unit values produce no WASM-stack values); `(u32, u64)` flattens to `[i32, i64]`.
- References to sized types and raw pointers → 1 `i32` (a byte address), regardless of pointee.
- Refs to DSTs (`&[T]` / `&mut [T]` / `&str` / `&mut str`) → fat: `[I32, I32]` (data ptr + length, 8 bytes in memory).
- Enums → `[I32]` (the address — enum values live in shadow-stack memory).
- `!` → empty vector.

`str` is layout-identical to `Slice<u8>` — kept as its own `RType::Str` variant so users get `&str` in error messages and so future UTF-8 invariants attach there.

Bare `[T]` / `str` are unsized — `byte_size_of` and `flatten_rtype` panic on them; the resolver currently doesn't reject them in sized positions, so users who write a bare slice/str in a let/param will hit a codegen panic rather than a friendly diagnostic (TODO).

## Memory layout

Used when a value lives on the shadow stack: tightly packed in declaration order, no alignment padding. `byte_size_of(struct, structs, enums)` is the sum of its fields' byte sizes. Same for tuples (positional). `byte_size_of` returns:
- 1/2/4/8/16 bytes for integer kinds.
- 1 byte for bool.
- 4 bytes for char.
- 4 bytes for refs to sized types and raw pointers.
- 8 bytes for refs to DSTs (fat).
- Sum-of-fields for structs and tuples.
- `4 + max(variant_payload_byte_size)` for enums.
- 0 for `!`.

All function params and return values that flatten to more than one WASM scalar become multi-value WASM signatures.

## Char literals

`'X'` / `'\n'` / `'¥'` / `'\u{2A}'`. The lexer disambiguates from lifetime tokens (`'a`) by lookahead — if the byte after `'` is `\` (escape) or ≥ 0x80 (multi-byte UTF-8 lead) or the second byte after `'` is `'` (single ASCII char), it's a char literal; otherwise lifetime.

Multi-byte source chars are decoded by a 1-4 byte UTF-8 reader that validates continuation bytes and rejects over-long encodings.

Recognized escapes: `\n` `\r` `\t` `\\` `\'` `\"` `\0` `\xNN` (ASCII only, 0x00..=0x7F) and `\u{HH..H}` (1-6 hex digits, valid Unicode codepoint). Lex-time codepoint validation: 0..=0x10FFFF excluding surrogates 0xD800..=0xDFFF.

Token form: `TokenKind::CharLit(u32)`; AST form: `ExprKind::CharLit(u32)`; type at use: `RType::Char`. Codegen pushes the codepoint as an `i32.const`. `as` casts route through `emit_int_to_int_cast` with the `char` side treated as `u32` (same wasm width).

## String literals

`"hello"` is `&'static str`. The lexer reads `"..."` and decodes the common Rust escape subset (`\n`, `\r`, `\t`, `\\`, `\"`, `\0`); other escapes and unterminated strings are lex errors. Source bytes ≥ 0x80 (UTF-8 continuation bytes) are copied through verbatim, so multi-byte characters survive lexing and the resulting `String` payload stays valid UTF-8.

At codegen, each literal is interned into a per-crate string pool (see codegen-machinery skill for the pool mechanics). The literal codegens to `i32.const data_addr; i32.const byte_len` — the fat-ref representation of `&str`.

## Structs

`struct NAME { field: Type, … }` or `struct NAME<T1, T2> { field: T1, … }` for generic structs. No tuple structs, no unit structs, no derive. Struct fields cannot be reference types but can use type params.

**Field-init shorthand:** `Foo { x }` parses as `Foo { x: x }` when the field name is followed by `,` or `}`; the value desugars to a synthetic `Var("x")` expression that name-resolves like any other. Mixing shorthand and explicit `name: expr` initializers in the same literal is fine.

## Enums (tagged unions)

`enum E { A, B(T1, T2), C { f: T1, g: T2 } }` — unit, tuple, and struct-shaped variants. Generics: `enum Option<T> { Some(T), None }`. `pub enum`. Variants share the enum's namespace; no per-variant `pub`.

Construction reuses existing nodes — `E::A` (parsed as a zero-arg `Call`), `E::A(args)` (`Call`), `E::A { f: e }` (`StructLit`) — and typeck disambiguates against the enum table via `lookup_variant_path`.

**Layout:** tagged-union, with the discriminant as an i32 at offset 0 and the variant payload (struct-shape: declaration order; tuple-shape: positional, smaller variants leave trailing payload bytes unused) starting at offset 4. `byte_size_of(enum) = 4 + max(variant_payload_byte_size)`.

Enum values live in shadow-stack memory; `flatten_rtype(enum) = [I32]` (the address).

**Variant construction:** allocates a fresh slot via `__sp -= byte_size_of(enum)`, writes disc + payload there, and yields the address.

**Nested enum payloads inlined:** when a variant's payload field is itself enum-typed, `store_flat_to_memory` memcpy's `byte_size_of(payload_enum)` bytes from the source address into the destination offset rather than storing the source address as a single i32 leaf.

The function epilogue restores `__sp` from a function-entry-saved local rather than `+= frame_size`, which lets construction sites allocate dynamically without a frame-layout pre-pass.

**sret return for enum-returning functions:** leading `i32` param is the caller-supplied destination address; before SP restore the function memcpys the constructed enum's bytes to that address (via `emit_memcpy`), then pushes the sret address as the i32 wasm result. Callers allocate the sret slot in their own frame before each enum-returning call.

## `!`-arm picking in if/match

`check_if_expr` (and `check_match_expr` analogously) returns the *non-`!` arm's* type when one arm diverges. So `if cond { panic!() } else { 42 }` types as the else arm's `u32`, not `!`. Without this picking, the if's recorded type would be `!` (zero flat scalars), the wasm `If` BlockType would be Empty, and downstream consumers expecting an `i32` would hit "values remaining on stack at end of block".
