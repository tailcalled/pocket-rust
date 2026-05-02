---
name: wasm-encoding
description: Use when modifying `src/wasm.rs` — pocket-rust's structured representation and byte encoder for WASM modules. Covers the section structure, the instruction set fragment supported, the LEB128 helpers, and the active-mode data segment limitation.
---

# `src/wasm.rs` — structured representation + byte encoding

Structured representation of WASM constructs:
- `Module` — the top-level container.
- `FuncType` — `(params, results)`.
- `Memory` — single linear memory; pocket-rust uses 1 page = 64 KiB, fixed.
- `Global` — mutable i32 globals (used for `__sp` and `__heap_top`).
- `Export` — exports user crate-root functions by name.
- `FuncBody` — function locals + instructions.
- `Data` — data segments (active-mode only).
- `Instruction` — covering `const`/`get`/`set`/`call`/`drop`, arithmetic on `i32`/`i64`, load/store with `align`/`offset` immediates, structured `Block`/`Loop`/`If`/`Else`/`End`/`Br`/`BrIf`/`Return`/`Unreachable`.

Plus byte encoding (`Module::encode`).

## Encoders

- uLEB128 / sLEB128 writers.
- Per-section encoders for type / function / memory / global / export / code / data sections.
- Section ordering follows the wasm spec.

## `BlockType` variants

- `Empty` — zero results.
- `Single(ValType)` — one wasm scalar (used for ≤64-bit ints, refs, raw pointers, bool, char).
- `TypeIdx(i)` — multi-value results (u128/i128, structs flattening to ≥2 scalars, generic `T` instantiated to such).

Multi-value FuncTypes are accumulated in `FnCtx.pending_types` during body codegen and appended to `wasm_mod.types` at function-emit-end; the typeidx is computed as `pending_types_base + position` so it stays correct across the append.

## Data segments

Active-mode data segments only (mode 0x00, memory index 0). Used by codegen to bake in the string-pool segment. No passive or declared segments.

## Imports

Codegen reserves wasm function index 0 for the host-imported `env.panic(ptr, len)` function, regardless of whether any `panic!` call is emitted. Module-defined functions occupy wasm idxs starting at `imports.len()` (currently 1).

## Pure data + encoding logic

`src/wasm.rs` is pure data + encoding logic. It doesn't know about pocket-rust's type system, AST, or codegen state — it's a faithful representation of WASM that codegen builds up and serializes at the end.
