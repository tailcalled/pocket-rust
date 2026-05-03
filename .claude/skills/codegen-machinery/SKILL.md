---
name: codegen-machinery
description: Use when modifying or debugging pocket-rust's WASM codegen (`src/codegen.rs`), pre-codegen mono expansion (`src/mono.rs`), or per-mono frame layout (`src/layout.rs`). Covers the shadow stack discipline, escape analysis, frame layout, `Storage` variants, `BaseAddr` semantics, the eager `MonoFn`/`MonoTable` expansion that runs before byte emission, the per-mono `compute_layout` pass, the string pool, and field/deref codegen patterns.
---

# codegen machinery

Entry: `pub fn emit(&mut wasm::Module, &Module, &StructTable, &FuncTable) -> Result<(), Error>`.

Appends to an existing `wasm::Module` rather than constructing a fresh one — same accumulating shape as `typeck::check`, so libraries' functions land in the WASM module first and user functions follow. Trusts that `typeck` and `borrowck` have already accepted the program; uses `unreachable!`/`expect` for cases the earlier passes would have caught. Reads typing artifacts from `expr_types` / `method_resolutions` / `call_resolutions` (NodeId-keyed) instead of recomputing types.

## Single-path codegen

There is **one** codegen path: `mono::expand` lowers each `MonoFn` to a `MonoBody` (an explicit, fully-resolved IR with no surface constructs left — `if`/`while`/`for`/`?`/`&&`/`||`/`if let`/`Index` all desugar to `Match`/`Loop`/`MethodCall` shapes), and `codegen::emit_function_concrete` invokes `codegen_mono_block` on it. There is no AST-driven codegen shadow path: if lowering fails or the per-variant `mono_supports_*` check rejects the body, the compile errors out (not a silent fallback). This was historically a dual-path arrangement; the AST codegen was removed once every reachable shape was Mono-supported.

Helpers shared by Mono codegen (kept after the AST removal): `codegen_pattern` + `bind_pattern_value` / `bind_pattern_ref` / `codegen_struct_pattern` / `codegen_variant_pattern` / `spill_match_scrutinee` / `stash_match_scrutinee` (pattern matching), `codegen_place_chain_load` (Field/TupleIndex chain reads — the Storage::Local branch extracts directly from the wasm-locals window via `flat_chain_offset`), `store_flat_to_memory` / `load_flat_from_memory` / `emit_memcpy` / `emit_base` (memory I/O), `emit_drops_for_locals_range` / `emit_drop_call_for_local` (drops), `codegen_builtin_128` + `emit_128_*` (128-bit arithmetic; routed through `emit_simple_builtin`'s wide-op dispatch), `emit_simple_builtin` (typed and arithmetic intrinsics), `emit_int_lit` / `emit_int_to_int_cast` / `push_int_*` (integer codegen primitives), `current_cf_depth` / `find_loop_frame` (loop break/continue depth), `block_type_for` / `pattern_uses_ref_binding` / `irrefutable_pattern` (small predicates).

## Pre-codegen passes

Two annotation passes run before any byte emission:

**`src/mono.rs` (`mono::expand`)**: walks every reachable function body — non-generic functions in the module tree, then template instances popped off the growing `MonoTable.entries` vector via an index cursor — and registers every required `(template_idx, concrete_args)` pair via `mono_table.intern`. Discovery covers the eight dispatch shapes that previously triggered lazy interning during emission:

- explicit `Call`s with `CallResolution::Generic { template_idx, type_args }`
- explicit `MethodCall`s with `MethodResolution.template_idx = Some(_)`
- trait-dispatched `MethodCall`s (`MethodResolution.trait_dispatch`) — re-runs `solve_impl_with_args` against the substituted recv to find the impl row
- `for x in iter` → `Iterator::next` via `solve_impl`
- `arr[i]` / `arr[range]` → `Index::index` (immutable contexts) OR `IndexMut::index_mut` (mutable contexts: `&mut`, assign LHS, `+=`-style compound assign whose autoref is `BorrowMut`). Lowering picks one based on the enclosing context — there is no `MonoExprKind::Index` variant; the AST `Index` node desugars at lowering time to `*<Index|IndexMut>::index{,_mut}(&base, i)` (a `Deref`-of-`MethodCall` place). The mutability flag threads through `lower_place(_, mutable)` and through `MethodCall` lowering's recv handling — `BorrowMut` recv_adjust reroutes its receiver through `lower_place(.., true)` so an inner `arr[i]` picks `index_mut`.
- `*expr` where `expr_types[inner.id]` is a struct (smart-pointer) → `Deref::deref`
- `let` value types and parameter types that are Drop → `Drop::drop`

After expand, `codegen::emit` iterates `mono_table.entries()` by index — entries can grow mid-loop if a body-walk dispatch site discovers a mono that expand missed, and the index walk picks them up.

**Two structs flow through the pipeline**:
- `MonoFnInput<'a>` — what `lower_to_mono` consumes. Owns substituted typeck artifacts (`expr_types` / `method_resolutions` / `call_resolutions` / `builtin_type_targets`), borrowck's `moved_places` / `move_sites` snapshots, signature info (`param_types` / `return_type`), `wasm_idx`, `is_export`, plus a `&'a Function` borrow. Built by `emit_function` (non-generics, from the FuncTable entry) or `build_mono_input_for_template` (template instances, with the typeck artifacts pre-substituted through the env). Discarded immediately after lowering — the typeck caches don't outlive the lowering call.
- `MonoFn` — what `emit_function_concrete` consumes. Owns the lowered `MonoBody`, signature (`name` / `param_types` / `return_type`), drop-state (`moved_places` / `move_sites`), `wasm_idx`, `is_export`. No AST reference, no typeck input caches, no lifetime parameter.

`lower_to_mono(input: &MonoFnInput) -> Result<MonoFn>`. Codegen never sees the AST.

`MonoState` (the codegen-side state struct) owns the `MonoTable` plus the string pool. `mono.intern(template_idx, args)` delegates to `mono_table.intern` for backward-compat with body-walking dispatch sites — these calls are now idempotent lookups against the pre-populated table.

**`src/layout.rs` (`layout::compute_mono_layout`)**: per-mono pass that walks the lowered `MonoBody` and produces `MonoLayout { binding_storage, binding_drop_action, binding_addressed, frame_size }`. Runs from inside `emit_function_concrete` (i.e. after lowering, before byte emission). Four phases:
1. `walk_block_address(&body.body, &mut addressed)` — walks the Mono IR. Every explicit `Borrow` / `BorrowOfValue` / `MethodCall` with `recv_adjust = BorrowImm/BorrowMut` / index-style synth call marks its root binding as addressed. The Mono IR makes every address-taking site syntactically visible (lowering already inserted the explicit `Deref` / synth `MethodCall` / `Borrow` shapes), so the walker doesn't infer anything — it just visits.
2. Drop-typed bindings auto-addressed: `is_drop(local.ty, traits) → addressed[binding_id] = true` (for the implicit `Drop::drop(&mut binding)` at scope-end).
3. Per-binding `BindingStorageKind` selection in BindingId order. Params + lets + non-pattern addressed bindings get `Memory { frame_offset }` (offsets allocated in BindingId order); pattern leaves and synthesized bindings get `MemoryAt` (codegen allocates the addr_local at bind/emission time, no fixed frame slot); unaddressed → `Local`.
4. Per-binding `DropAction` via `compute_drop_action`.

`BindingStorageKind` is the *decision* layout makes; codegen fills in emission-time details (`wasm_start` for `Local`, `addr_local` for `MemoryAt`). Codegen reads `ctx.binding_storage[binding_id]` (BindingId-keyed) directly — single source of truth, no NodeId-based caches. `layout.frame_size` is what the prologue subtracts from `__sp`. Param storage is the prefix `binding_storage[..param_count]` since lowering declares params first.

Per-binding `DropAction` (also in `src/layout.rs`) is computed via `compute_drop_action(name, ty, moved_places, traits)` at every `LocalBinding` decl site and stashed on the binding directly. Codegen's drop emission (`emit_drops_for_locals_range`) and flag-allocation paths read `LocalBinding.drop_action` instead of recomputing `is_drop` + move-status — the decision lives in one place. See the `drop-and-destructors` skill for the action variants.

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

Each `(template, concrete type_args)` pair becomes a separate WASM function with a fresh idx. Discovery is eager — `mono::expand` walks every reachable body before codegen begins, registering each pair via `MonoTable.intern` (allocates idx on first call, returns existing idx on repeat). Codegen then iterates `mono_table.entries()` by index to emit each body via `emit_monomorphic` → `build_mono_for_template` → `emit_function_concrete(&MonoFn, ...)`.

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
