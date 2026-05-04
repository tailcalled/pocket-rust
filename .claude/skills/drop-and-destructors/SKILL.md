---
name: drop-and-destructors
description: Use when working on Drop semantics — destructor calls, scope-end behavior, drop ordering, drop flags, partial-move rejection, Drop/Copy mutual exclusion, or pattern-bound Drop bindings. Covers the full pipeline from `is_drop` queries through codegen's drop-emission machinery.
---

# Drop machinery (T4 + T4.5 + T4.6 + T4.7)

`pub trait Drop { fn drop(&mut self); }` defined in `lib/std/ops.rs` (re-exported as `std::Drop`).

Two predicates with different jobs:
- `is_drop(rt, traits)` — direct-impl question. `solve_impl(drop_trait_path(), rt, ...)` where `drop_trait_path()` returns `["std", "ops", "Drop"]`. Used by impl validation (Drop/Copy mutual exclusion) and inside the drop walker to decide whether to call the user's `Drop::drop` method on a value before recursing into its fields.
- `needs_drop(rt, structs, enums, traits)` — destruction-required question. True if `is_drop(rt)` OR any structurally contained sub-place (struct field, enum variant payload, tuple element) needs_drop. Used by `compute_drop_action` and by the auto-address pass — aggregates with Drop fields participate in drop emission even when they don't themselves impl Drop.

## Drop / Copy mutual exclusion

`register_trait_impl` rejects an impl of one when the other already exists for the same target. The two semantics are fundamentally incompatible: Copy means trivial bitwise duplication; Drop means a meaningful destructor must run on every value at end of scope.

## Address-taken bindings — Drop requires shadow-stack slots

Drop's destructor takes `&mut self`, so the binding must have an address. The drop walker also computes per-field addresses by adding byte offsets to the binding's base. Both reasons make every `needs_drop` binding (lets and function params) address-taken — `compute_mono_layout`'s phase-2 pass sets `addressed[binding_id] = true` for any binding whose type satisfies `needs_drop`. Frame layout then allocates a `Storage::Memory { frame_offset }` slot for it.

## Scope-end drop emission

At scope end, `emit_drops_for_locals_range(ctx, from, to)` walks `ctx.locals[from..to]` in **reverse declaration order**, emitting `<T as Drop>::drop(&mut binding)` for each Drop-typed binding (skipping non-Drop, skipping non-addressable storage, consulting move state for skip/flag — see below).

The function is called from:
- `codegen_unit_block_stmt` (statement-only inner blocks)
- `codegen_block_expr` (block expressions with tails — saves the tail value to fresh locals before drops then reloads, so the return value survives the destructors)
- Function body end (before SP epilogue)
- `Return` codegen (mirrors fn-end: stash value, drop in-scope bindings, restore SP, emit `Return`)
- `break`/`continue` codegen (drops bindings allocated since the loop boundary)

`emit_drop_call_for_local` materializes the binding's address into a fresh wasm i32 local and hands off to `emit_drop_walker`. Two storage shapes carry an address:
- `Storage::Memory { frame_offset }` — push `frame_base_local` (post-prologue SP), add offset.
- `Storage::MemoryAt { addr_local }` — push the addr from the wasm local directly. Used for dynamically-allocated slots from `codegen_pattern` (including pattern bindings auto-addressed for Drop).

Use `frame_base_local`, not live `__sp`: by the time scope-end drops fire, body may have drifted `__sp` via literal-borrow temps, sret slots, or enum construction.

`emit_drop_walker(ctx, ty, addr_local)` is the recursive workhorse. Mirrors rustc's `core::ptr::drop_in_place::<T>` synthesis:
1. If `is_drop(ty)`: resolve the user's `Drop::drop` via `solve_impl` + `find_trait_impl_method` (monomorphizing the impl-method template if needed), push `addr_local`, `Call`. The user's body sees pre-drop field values.
2. Then walk the value's structure and recurse on each `needs_drop` sub-place:
   - **Struct / tuple** — declaration-order field walk; per field, allocate a fresh i32 local set to `addr_local + byte_offset` (via `emit_address_at_offset`), recurse with the field's substituted type.
   - **Enum** — `emit_enum_variant_walker` collects variants whose payload contains any `needs_drop` field; loads disc once into a wasm local; emits a chain of `if disc == N { ... }` blocks (one per walking variant, no else — fall-through is fine). Inside each block, walks the variant's payload by field byte-offset (payload starts at offset 4, after the i32 disc).
3. Aggregates with no Drop fields produce no walker work — `needs_drop` is false, `compute_drop_action` returns `Skip` upstream.

Bindings in `Storage::Local` (wasm locals) silently skip drop emission — `needs_drop` bindings should never end up there (the layout auto-address pass should have marked them).

## Move-aware drops (T4.6)

Borrowck allows whole-binding moves of Drop values and rejects only partial moves — Drop's destructor runs over the whole value, so leaving a hole would be unsound. `partial-move-of-Drop` produces a Drop-specific error message at borrowck time.

When `let _y: Logger = l;` is a whole-binding move of a Drop value, borrowck records it; codegen then **skips `l`'s implicit scope-end drop**, and only `_y`'s drop fires.

## Three-valued move state + drop flags (T4.7)

`MoveStatus = Moved | MaybeMoved` (Init implicit by absence). Straight-line code only ever produces `Moved`; `MaybeMoved` arises at if-merge points where one arm moved a binding and the other didn't.

Borrowck snapshots:
- `FnSymbol.moved_places: Vec<MovedPlace>` — per-place final status.
- `FnSymbol.move_sites: Vec<(NodeId, name)>` — every whole-binding move site.

Codegen consults both via the per-binding `DropAction` precomputed in `layout::compute_drop_action(name, ty, moved_places, structs, enums, traits)`:
- **Skip** — `!needs_drop(ty)` OR moved on every path. No drop emitted.
- **Always** — `needs_drop(ty)` and never moved (no entry in `moved_places`). Unconditional drop at scope end (the walker handles direct-Drop vs aggregate dispatch).
- **Flagged** — `needs_drop(ty)` and `MaybeMoved` (moved on some paths). Allocate an i32 wasm local as the drop flag, init `1` at the binding's let-stmt (or fn entry for params), emit `i32.const 0; local.set flag` at every move site listed in `move_sites`, and gate the scope-end drop call with `local.get flag; if; <drop>; end`.

The flag is whole-binding granularity. Pocket-rust does not yet track per-field drop state for partial moves out of `needs_drop` aggregates — fix the partial-move story before relying on it.

Every `LocalBinding` carries its precomputed `DropAction` directly (stashed at decl time via `layout::compute_drop_action`). `emit_drops_for_locals_range` reads `ctx.locals[i].drop_action` instead of recomputing `needs_drop` + move-status per iteration. Same goes for the let-stmt and param flag-allocation sites — they consult `drop_action` directly.

## Pattern-bound bindings interaction

`mark_drop_bindings_addressed` walks let-stmt patterns. Two paths depending on the let shape:

**Tuple destructure (`let (a, b) = …;`).** Any leaf binding whose type is Drop forces `let_addressed[value_id] = true` via `let_pattern_has_drop_leaf`. The codegen path then frame-spills the whole tuple at the let's offset and registers each element binding as `Storage::Memory { frame_offset: base + sub_offset }`, giving the scope-end drop machinery a real address per element. Same path also kicks in when escape analysis flags any element as borrow-rooted (`&binding…`).

**let-else.** `auto_address_drop_pattern_bindings` walks the pattern's leaf Bindings and sets `pattern_addressed[leaf.id] = true` for Drop-typed leaves. `codegen_pattern`'s existing addressed-binding path then allocates a `Storage::MemoryAt { addr_local }` shadow-stack slot per Drop leaf.

Without these passes, pattern bindings would land in `Storage::Local` and the scope-end drop machinery would silently skip them — the kind of silent-deferral bug the project rules forbid. Tests in `tests/std/drop.rs` cover the full interaction matrix:
- `drop_tuple_destructure_runs_both_drops` — both elements drop.
- `drop_destructure_order_is_reverse_decl` — drops fire in reverse decl order.
- `drop_tuple_destructure_partial_move_drops_remaining` — moving one binding doesn't suppress the other's drop.
- `drop_let_else_match_drops_binding` — let-else success-path bindings drop.
- `drop_pattern_addr_taken_borrows_work` — `&_a` / `&_b` borrows on destructured bindings work.
- `drop_destructure_borrow_conflict_is_rejected` — mutable+shared overlap on a destructured binding rejected.

## `mem::drop` wrapper

`lib/std/mem.rs` defines `pub fn drop<T>(_x: T) {}` — mirrors `std::mem::drop`. Consumes T by value so the existing scope-end Drop machinery runs `T::drop` for Drop types and is a no-op for non-Drop types.

## Stdlib Drop-using types

- `Vec<T>` — Drop impl calls `mem::drop` on each element (which runs T::drop if Drop) then `¤free`s the buffer.
- `Box<T>` — Drop impl runs T::drop (if Drop) then frees the buffer; uses a null-ptr sentinel in the buffer-pointer field to suppress the Drop impl when ownership is handed off (`into_raw`/`into_inner`/`leak`).
