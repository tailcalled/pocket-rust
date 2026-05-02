---
name: drop-and-destructors
description: Use when working on Drop semantics — destructor calls, scope-end behavior, drop ordering, drop flags, partial-move rejection, Drop/Copy mutual exclusion, or pattern-bound Drop bindings. Covers the full pipeline from `is_drop` queries through codegen's drop-emission machinery.
---

# Drop machinery (T4 + T4.5 + T4.6 + T4.7)

`pub trait Drop { fn drop(&mut self); }` defined in `lib/std/ops.rs` (re-exported as `std::Drop`). `is_drop(rt, traits)` queries via `solve_impl(drop_trait_path(), rt, ...)` where `drop_trait_path()` returns the canonical `["std", "ops", "Drop"]`.

## Drop / Copy mutual exclusion

`register_trait_impl` rejects an impl of one when the other already exists for the same target. The two semantics are fundamentally incompatible: Copy means trivial bitwise duplication; Drop means a meaningful destructor must run on every value at end of scope.

## Address-taken bindings — Drop requires shadow-stack slots

Drop's destructor takes `&mut self`, so the binding must have an address. Codegen marks every Drop-typed binding (lets and function params) as address-taken via `mark_drop_bindings_addressed`, which sets `let_addressed[value_id] = true` (lets) or `param_addressed[idx] = true` (params). Frame layout then allocates a `Storage::Memory { frame_offset }` slot for it.

## Scope-end drop emission

At scope end, `emit_drops_for_locals_range(ctx, from, to)` walks `ctx.locals[from..to]` in **reverse declaration order**, emitting `<T as Drop>::drop(&mut binding)` for each Drop-typed binding (skipping non-Drop, skipping non-addressable storage, consulting move state for skip/flag — see below).

The function is called from:
- `codegen_unit_block_stmt` (statement-only inner blocks)
- `codegen_block_expr` (block expressions with tails — saves the tail value to fresh locals before drops then reloads, so the return value survives the destructors)
- Function body end (before SP epilogue)
- `Return` codegen (mirrors fn-end: stash value, drop in-scope bindings, restore SP, emit `Return`)
- `break`/`continue` codegen (drops bindings allocated since the loop boundary)

`emit_drop_call_for_local` handles two storage shapes:
- `Storage::Memory { frame_offset }` — push `frame_base_local` (post-prologue SP), add offset.
- `Storage::MemoryAt { addr_local }` — push the addr from the wasm local directly. Used for dynamically-allocated slots from `codegen_pattern` (including pattern bindings auto-addressed for Drop).

Use `frame_base_local`, not live `__sp`: by the time scope-end drops fire, body may have drifted `__sp` via literal-borrow temps, sret slots, or enum construction.

Bindings in `Storage::Local` (wasm locals) silently skip drop emission — Drop bindings should never end up there (the auto-address pass should have marked them).

## Move-aware drops (T4.6)

Borrowck allows whole-binding moves of Drop values and rejects only partial moves — Drop's destructor runs over the whole value, so leaving a hole would be unsound. `partial-move-of-Drop` produces a Drop-specific error message at borrowck time.

When `let _y: Logger = l;` is a whole-binding move of a Drop value, borrowck records it; codegen then **skips `l`'s implicit scope-end drop**, and only `_y`'s drop fires.

## Three-valued move state + drop flags (T4.7)

`MoveStatus = Moved | MaybeMoved` (Init implicit by absence). Straight-line code only ever produces `Moved`; `MaybeMoved` arises at if-merge points where one arm moved a binding and the other didn't.

Borrowck snapshots:
- `FnSymbol.moved_places: Vec<MovedPlace>` — per-place final status.
- `FnSymbol.move_sites: Vec<(NodeId, name)>` — every whole-binding move site.

Codegen consults both:
- **Init** (no entry) → unconditional drop at scope end.
- **Moved** (every path moved it) → no drop emitted.
- **MaybeMoved** (some paths moved, some didn't) → allocate an i32 wasm local as the drop flag, init `1` at the binding's let-stmt (or fn entry for params), emit `i32.const 0; local.set flag` at every move site listed in `move_sites`, and gate the scope-end drop call with `local.get flag; if; <drop>; end`.

`needs_drop_flag(moved_places, name, rt, traits)` — true iff the binding is Drop AND its `moved_places` entry is `MaybeMoved`. Drives flag-allocation at let-stmt codegen.

`binding_move_status(moved_places, name)` — looks up a binding's whole-binding move status. Returns `None` for `Init`. Drives the skip/unconditional/flagged decision at scope end.

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
