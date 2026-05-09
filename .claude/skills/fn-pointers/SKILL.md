---
name: fn-pointers
description: Use when working with function-pointer types `fn(T) -> R` — coercion of a bare fn-item name to an FnPtr value, indirect calls through an FnPtr local, the funcref-table-slot mechanism, and how `CallIndirect` is emitted. Phase 1 of the dyn-trait roadmap.
---

# Function pointers — `fn(T) -> R`

Pocket-rust's function-pointer types are a single-i32 runtime value (an index into the module's funcref table). They unlock the indirect-call WASM infrastructure (`call_indirect`, Table + Element sections) that subsequent dyn-trait phases reuse for vtables.

## Surface syntax

`fn(T1, T2, ..., Tn) -> R` in any type position. `R` defaults to `()` if `-> R` is omitted.

```
fn double(x: u32) -> u32 { x + x }
fn answer() -> u32 {
    let f: fn(u32) -> u32 = double;
    let g = f;          // FnPtr is Copy
    f(21) + g(0)        // both call through the funcref table
}
```

Coercion fires at any unification site that wants `RType::FnPtr`: let-annotations, fn args, struct field initializers, fn return values.

## Coercion mechanics

A bare `Var(name)` expression that resolves to a non-generic fn item — instead of a local of the same name — produces an `RType::FnPtr` shaped from the fn's `param_types` + `return_type`. The lookup happens after the const fallback in `check_var` (`src/typeck/mod.rs`), and records the FuncTable callee_idx on `CheckCtx.fn_item_addrs[expr.id]`.

Generic fn items (`fn id<T>(x: T) -> T`) are rejected with "cannot take address of generic fn `…` as a fn pointer" — there's no syntax for higher-order type-arg threading yet. Specifying turbofish (`id::<u32>`) at the address site would fix this; deferred.

## Call-site dispatch

`f(args)` where `f` is a local of `RType::FnPtr` routes through `check_indirect_call`:
- arity + arg-type checks against the FnPtr's signature,
- `PendingCall::Indirect { callee_local_name, param_infers, ret_infer }` recorded on `call_resolutions[expr.id]`.

End-of-fn finalization lowers it to `CallResolution::Indirect { callee_local_name, fn_ptr_ty }` with concrete RTypes.

## Mono lowering

Two new `MonoExprKind` variants:

- `FnItemAddr { wasm_idx }` — produced when lowering a `Var` whose `fn_item_addrs[expr.id]` is `Some(callee_idx)`. Carries the wasm function index.
- `CallIndirect { callee, args, fn_ptr_ty }` — produced from `CallResolution::Indirect`. The `callee` lowers to a `MonoExprKind::Local(binding_id, _)` reading the FnPtr's i32 slot value.

`MonoFnInput` carries `fn_item_addrs: Vec<Option<usize>>`, copied from `FnSymbol.fn_item_addrs` / `GenericTemplate.fn_item_addrs`.

## Codegen + WASM emission

`MonoState` (in `src/codegen.rs`) accumulates two new things alongside the string pool:
- `pending_table_slots: Vec<u32>` — wasm function indices for this crate's FnPtr coercions.
- `table_base_offset: u32` — the existing `wasm_mod.func_table.len()` at MonoState construction. Slot indices returned by `intern_table_slot` are absolute (`table_base_offset + position`), so multi-crate codegen stays correct as long as later crates' contributions are appended.

`intern_table_slot(wasm_idx)` deduplicates: repeated `&id` coercions of the same fn share a single slot.

Codegen emission:
- `FnItemAddr { wasm_idx }` → `i32.const <intern_table_slot(wasm_idx)>`.
- `CallIndirect { callee, args, fn_ptr_ty }` → emit args, emit callee (the i32 slot), then `call_indirect typeidx`. The typeidx comes from `intern_pending_func_type` against a `wasm::FuncType` built by flattening the FnPtr signature (with sret-prefix-i32 for enum returns).

At end of `emit()` for a crate, `mono.pending_table_slots` is appended to `wasm_mod.func_table`, which materializes the Table + Element sections at encode time (see `wasm-encoding`).

## Layout / borrowck integration

- `RType::FnPtr` flattens to one `i32`, byte_size 4. Layouts behave like a scalar.
- `is_copy_with_bounds` returns `true` unconditionally for `FnPtr`. There's no `impl Copy for fn(...)` in source (no fn-ptr-shaped type pattern), so the special-case lives in `src/typeck/types.rs`.
- Borrowck recognizes `Var` expressions with a recorded `fn_item_addrs[expr.id]` and lowers them to a placeholder constant operand (`OperandKind::ConstInt(0)`) — the FnPtr value is Copy and carries no place to track. The actual slot value comes from codegen's `intern_table_slot`.
- Indirect calls in borrowck synthesize a single-segment `CallTarget::Path([callee_local_name])` placeholder; arg-borrow propagation still happens through the standard call-site machinery.

## Open follow-ups

- Generic fn items as fn-pointers (`let f: fn(u32) -> u32 = id::<u32>;`) — needs turbofish-on-bare-name parsing and a mono key per (callee, type_args).
- Method-pointer addressing (`Foo::method`) — same mechanism, different name resolution.
- `unsafe fn(...) -> R` syntax + safeck routing — Phase 2 may need this for `dyn UnsafeTrait`.
- Fat-fn-pointer with environment (closure types) — out of scope; closures take a different path through `closure-and-fn-traits`.
