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
- `func_table: Vec<u32>` — function-pointer / vtable backing storage. Each entry is a wasm function index (the same value a `Call(idx)` would use, after the import offset). Drives both the Table and Element sections at encode time.
- `Instruction` — covering `const`/`get`/`set`/`call`/`call_indirect`/`drop`, arithmetic on `i32`/`i64`, load/store with `align`/`offset` immediates, structured `Block`/`Loop`/`If`/`Else`/`End`/`Br`/`BrIf`/`Return`/`Unreachable`.

Plus byte encoding (`Module::encode`).

## Encoders

- uLEB128 / sLEB128 writers.
- Per-section encoders for type / function / table / memory / global / export / element / code / data sections.
- Section ordering follows the wasm spec: Type(1) → Import(2) → Function(3) → Table(4) → Memory(5) → Global(6) → Export(7) → Element(9) → Code(10) → Data(11).

## Function table + Element section

Indirect calls (function pointers, dyn-trait vtables) dispatch through a single funcref table populated at module-init time. The mechanism:

- `Module::intern_table_slot(wasm_idx) -> u32` — call to reserve a table slot for a function. Returns the slot index; deduplicates so repeated calls with the same `wasm_idx` collapse to one slot. Codegen calls this when it lowers a fn-pointer coercion or builds a vtable entry.
- A non-empty `func_table` triggers two sections at encode time:
  - **Table section (id 4)** — declares one funcref table sized `min == max == func_table.len()`. The table never grows at runtime.
  - **Element section (id 9)** — one MVP active-mode segment (flag byte `0x00`), placing every entry at offset 0 of table 0. Encoded as `0x00 | i32.const 0 ; end | vec(funcidx)`.
- `Instruction::CallIndirect { type_idx, table_idx }` (opcode `0x11`) — stack discipline: args first, then the i32 table slot, then the instruction; results land per the referenced FuncType. `table_idx` is always 0 today (only one funcref table); kept as a field for forward-compat.

If `func_table` is empty, neither section is emitted, so existing module shapes are byte-identical to before.

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
