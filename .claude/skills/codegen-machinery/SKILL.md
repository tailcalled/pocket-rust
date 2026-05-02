---
name: codegen-machinery
description: Use when modifying or debugging pocket-rust's WASM codegen (`src/codegen.rs`). Covers the shadow stack discipline, escape analysis, frame layout, `Storage` variants, `BaseAddr` semantics, monomorphization worklist, the string pool, and field/deref codegen patterns.
---

# codegen machinery

Entry: `pub fn emit(&mut wasm::Module, &Module, &StructTable, &FuncTable) -> Result<(), Error>`.

Appends to an existing `wasm::Module` rather than constructing a fresh one — same accumulating shape as `typeck::check`, so libraries' functions land in the WASM module first and user functions follow. Trusts that `typeck` and `borrowck` have already accepted the program; uses `unreachable!`/`expect` for cases the earlier passes would have caught. Reads typing artifacts from `expr_types` / `method_resolutions` / `call_resolutions` (NodeId-keyed) instead of recomputing types.

## Shadow stack — real pointers in linear memory

A reference value is an `i32` byte address into the module's single linear memory (1 page = 64 KiB, fixed). Two `mut i32` globals are seeded by `lib.rs`:
- index 0: `__sp` — initialized to 65536; shadow stack grows downward for spilled bindings, enum construction, sret slots, literal-borrow temps.
- index 1: `__heap_top` — initialized to 8 (or higher when string-pool data is baked in); heap grows upward, bumped by `¤alloc`, never reclaimed by `¤free`.

Heap and shadow stack share the same 64 KiB page and collide silently if either grows too far — there's no OOM check.

## Per-function flow

1. **Escape analysis.** A pre-pass over the body marks each binding (param or `let`) as *addressed* if any `&binding…` / `&mut binding…` chain takes its address. Drop-typed bindings are also auto-addressed via `mark_drop_bindings_addressed`.
2. **Frame layout.** Addressed bindings get fixed byte offsets within the function's frame; `frame_size` is the sum of their `byte_size_of`s.
3. **Prologue / epilogue.** If `frame_size > 0`: `__sp -= frame_size` on entry; on exit, `__sp` is restored from a function-entry-saved local (so dynamic allocations during the body — enum construction, sret slots, literal-borrow temps — get reclaimed too). Spilled params are also copied from their incoming WASM-local slots into the frame at this point. The spill-cursor that names each user-param's incoming wasm local starts at `1` for sret-returning functions (wasm local 0 holds the caller-supplied sret_addr) and at `0` otherwise — without this offset, the prologue copies sret_addr into the first spilled-param slot and corrupts `self`. After the prologue, `__sp` is captured into a wasm local `frame_base_local`; **all spilled-binding reads/writes/borrows use this stable base, not live `__sp`**, since `__sp` drifts during the body and bindings sit at fixed offsets relative to "frame top". Without this, e.g. a `while` loop whose cond does `i < N` (which desugars to `i.lt(&N)` — `&N` is a literal-borrow that does `__sp -= 4`) would shift the apparent address of every spilled binding by 4 bytes per iteration. Scope-end `Drop::drop` calls also use `frame_base_local` to compute the binding's address (not live `__sp`); same invariant.
4. **Spilled bindings.** Live in memory at `frame_base + frame_offset`. Reads emit per-leaf `iN.load` ops; writes emit per-leaf `iN.store` ops, with the byte offset folded into the load/store immediate. `&binding.field…` evaluates to `frame_base + frame_offset + chain_byte_offset` (an i32). `BaseAddr::StackPointer` in codegen names this base — `emit_base` lowers it to `LocalGet(frame_base_local)`. Dynamic allocations (enum/sret/literal-borrow) still use live `__sp` via direct `GlobalGet(SP_GLOBAL)` since they want a fresh slot below the current top.
5. **Non-spilled bindings.** Stay in WASM locals as flat scalars. References are typically non-spilled (just an i32 in a WASM local); the exception is when escape analysis sees `&r.field…` and conservatively spills `r` even though only `r`'s value is needed — codegen handles this via `ref_pointee_addr_local`, which loads the i32 from the spilled slot once and reuses it.
6. **Call sites.** Reference params are passed as a single i32; no out-parameter rewriting, no writeback dance — mutation through `&mut r` is a real `iN.store` against the address `r` holds.

## `Storage` variants

A `LocalBinding` carries one of:
- `Storage::Local { wasm_start, flat_size }` — value sits in a contiguous range of wasm locals as flat scalars. Fast path; no address.
- `Storage::Memory { frame_offset }` — value sits at `frame_base_local + frame_offset` in shadow-stack memory. Has a stable address; reads/writes via per-leaf load/store with offset folded into the immediate.
- `Storage::MemoryAt { addr_local }` — value sits at the address held in `addr_local` (a wasm i32 local). Used by `codegen_pattern` for addressed pattern bindings (including auto-addressed Drop pattern leaves) and for dynamically-allocated slots.

`emit_drop_call_for_local` and any borrow-of-binding code path must handle both `Memory` and `MemoryAt` — they both expose addressable storage.

## `BaseAddr`

- `BaseAddr::StackPointer` — `frame_base_local` (post-prologue SP). Use for spilled bindings' fixed slots.
- `BaseAddr::WasmLocal(idx)` — i32 wasm local holding an address. Use for dynamically-allocated slots (enum sret, MemoryAt bindings, intermediate addresses).

`emit_base` lowers them to `LocalGet`.

## Monomorphization

Each `(template, concrete type_args)` pair becomes a separate WASM function with a fresh idx, populated via a FIFO worklist drained at the end of `codegen::emit`. `ctx.mono.intern(template_idx, concrete_types)` returns the wasm function index, allocating a new entry on first use.

Methods on generic impls register as templates whose `type_params = impl's params + method's own params` (in that order); method-call resolution binds the impl-bound slots from the receiver's struct `type_args` and allocates fresh inference vars for the method's own slots.

Cross-crate generics work — generic templates in `FuncTable.templates` own a `Function` clone, so a library's templates outlive the library's `Module`.

## String pool

Each string literal is interned (by payload) into a per-crate string pool that the `emit` driver flushes into a single active-mode wasm data segment at memory offset `STR_POOL_BASE = 8` (just past the null-territory bytes). After flushing, `__heap_top`'s init constant is bumped to `STR_POOL_BASE + total_pool_size` so the heap doesn't collide with the baked-in data.

Multi-crate compiles (stdlib + user) accumulate into one segment — each call to `codegen::emit` reads the existing segment's size as its base offset, appends, and re-bumps `__heap_top`.

The literal itself codegens to `i32.const data_addr; i32.const byte_len` — the fat-ref representation of `&str`.

## Field access codegen

For `expr.field`, the base is fully evaluated onto the stack (always — there's no place-expression optimisation yet), then a stash-and-restore over freshly allocated locals extracts the desired range. Each `FieldAccess` allocates new temp locals; we don't reuse. Chains like `expr.a.b.c` produce one stash-restore per `.`; an obvious future optimisation is to fold a chain into a single extraction at the cumulative offset/size.

Field access through a reference becomes a direct `iN.load` against the ref's i32 with the field offset folded into the load's immediate.

## `&*ptr` / `&mut *ptr` — place borrow

`&*ptr` / `&mut *ptr` is a place borrow — codegen evaluates `ptr` (pushes its i32 address) and uses that directly as the borrow's value, with no fresh shadow-stack slot. Without this, `&mut *raw_ptr` would copy the pointee through a temp, and writes through the resulting `&mut T` would target the temp instead of the raw pointer's destination — breaking idioms like `Vec::get_mut` that turn a computed `*mut T` back into a `&mut T`. The general "non-place inner" path (`&42`, `&foo()`) still spills the value through a fresh slot since there's no existing addressable storage to point at.

## Layout helpers (`typeck::types`)

- `byte_size_of(rtype, structs, enums)` — byte size for `frame_size` accounting and chain offsets (1/2/4/8/16 bytes for ints, 4 for refs, sum-of-fields for structs, 4 + max-payload for enums).
- `flatten_rtype(rtype, structs, out)` — flat WASM scalars for non-spilled bindings. Refs to sized types flatten to `[I32]` (ABI), not the pointee's shape. Refs to DSTs (`&[T]`/`&str`) flatten to `[I32, I32]` (ptr, len).

## Multi-value FuncTypes

Multi-value FuncTypes (for if/match results that yield ≥2 wasm scalars) are accumulated in `FnCtx.pending_types` during body codegen and appended to `wasm_mod.types` at function-emit-end; the typeidx is computed as `pending_types_base + position` so it stays correct across the append.

## Trait-impls on non-Path targets

For trait impls on non-Path targets (`impl Trait for (u32, u32)`, `impl<T> Trait for &T`, `impl Trait for bool`), typeck synthesizes the method-path prefix `__trait_impl_<idx>` where `idx` is the impl's row in `TraitTable.impls`. Codegen recovers that idx via `find_trait_impl_idx_by_span(traits, file, span)` so it can mirror typeck's prefix and emit each method (otherwise the registered FnSymbols would be dropped on the floor and any reference to them would dangle).

For inherent impls on raw-pointer targets, setup allocates a synth idx (recorded as `(file, span)` in `FuncTable.inherent_synth_specs`); the prefix is `__inherent_synth_<idx>`. Body-check and codegen recover the same idx via `find_inherent_synth_idx`.
