---
name: dyn-trait
description: Use when working with trait objects `&dyn Trait` / `&mut dyn Trait` — the parser/AST/RType plumbing, lazy object-safety check, `&T → &dyn Trait` coercion mechanics, vtable storage in the data segment, and `DynMethodCall` dispatch through `call_indirect`. Phase 2 of the dyn-trait roadmap. Phase 1 (`fn-pointers`) is the foundation.
---

# Trait objects — `&dyn Trait` / `&mut dyn Trait`

A trait object is a type-erased value behind a fat reference: a data pointer (the concrete value's address) plus a vtable pointer (a per-(trait, type) static array of funcref-table-slot indices). Method calls on a `&dyn Trait` receiver dispatch through the vtable via `call_indirect`.

## Surface syntax

`dyn TraitA + TraitB + 'a` in any type position. The DST itself is unsized; only valid behind `&` / `&mut` (Phase 3 adds `Box<dyn Trait>`).

```
trait Show { fn show(&self) -> u32; }
struct A { v: u32 } impl Show for A { fn show(&self) -> u32 { self.v } }
fn ping(s: &dyn Show) -> u32 { s.show() }
fn answer() -> u32 {
    let a = A { v: 42 };
    ping(&a)        // `&A` coerces to `&dyn Show`
}
```

The lexer emits `TokenKind::Dyn`; `parse_type` recognizes it (placed between the `Impl` arm and the `Bang` arm) and emits `TypeKind::Dyn { bounds, lifetime }`.

## RType

`RType::Dyn { bounds: Vec<Vec<String>>, lifetime: LifetimeRepr }`. Each bound is a canonical trait path (resolved via the use scope's explicit imports; module-relative fallback otherwise). Two `Dyn` types are equal iff their bound paths match in order — pocket-rust doesn't normalize multi-trait order today.

DST: `byte_size_of(Dyn) = panic`, `flatten_rtype(Dyn) = panic`. The valid-only-behind-ref invariant is enforced by every consumer hitting the `Ref { inner: Dyn(_), .. }` arm of `flatten_rtype`/`collect_leaves`/`byte_size_of`, which produces two `i32` leaves (data ptr + vtable ptr) — same shape as fat slice/str refs.

## Object-safety check

Lazy: fires at every `&T → &dyn Trait` coercion site and at every dyn-method-call site. Lives in `src/typeck/dyn_safety.rs`. Three rules per method (across the trait + supertrait closure):

1. **Receiver shape** must be `&self` or `&mut self`. By-value `self` and methods without a receiver are rejected.
2. **No method-level type parameters.** Each `<U>` would need a separate vtable entry per monomorphization.
3. **No `Self` outside the receiver.** The erased object can't witness the concrete type, so `fn eq(&self, other: Self)` is rejected.

Errors name the offending method + which clause failed (e.g. `cannot coerce to \`dyn Foo\`: method \`take\` takes \`self\` by value`).

## Coercion: `&T → &dyn Trait`

Recorded by `coerce_at(ctx, expr_id, actual, expected, span)` — a typeck helper that wraps `Subst::coerce` with a special-case detector for the `Ref<T> → Ref<Dyn>` shape. Called at four sites today:

- let-stmt RHS vs annotation
- function call args vs param types
- function return-tail vs declared return
- function `return EXPR` value

When the shape matches:

1. Run `dyn_safety::check_object_safety` for each bound trait.
2. Verify `solve_impl(bound, T)` succeeds for each bound.
3. Disallow `&T → &mut dyn Trait` (mutability mismatch); allow `&mut T → &dyn Trait` (downgrade) and `&mut T → &mut dyn Trait` (preserved).
4. Record on `ctx.dyn_coercions[expr_id]` a `DynCoercion { src_concrete_ty, trait_paths }`.

The expression's recorded type stays `Ref<Dyn>` (the coerced-to type); mono reads the matching `dyn_coercions` entry to wrap the lowered expression in `MonoExprKind::RefDynCoerce`.

## Method dispatch on `&dyn Trait`

`recv.method(args)` where `recv: &dyn Trait` (or `&mut dyn Trait`):

1. `check_method_call` detects the `Ref<Dyn>` recv shape and routes to `check_dyn_method_call`.
2. Find `method` in the trait's `methods` list (Phase 2 v1: no supertrait method dispatch).
3. Verify the method's receiver shape matches the dyn-ref's mutability.
4. Type-check args against the method's signature (object-safety guarantees no `Self` substitution needed).
5. Record `DynMethodDispatch { trait_path, method_idx, method_param_types, method_return_type, recv_mut }` on `ctx.dyn_method_calls[expr_id]`.

Codegen emits args + recv data ptr + load funcref slot from `vtable[method_idx*4]` + `call_indirect`, with a typeidx interned from the method's signature (prefixed with `i32` for the `&self` arg, plus `i32` sret prefix for enum returns).

## Vtable storage

Lives in `MonoState`:

- `vtables: Vec<((Vec<String>, RType), u32)>` — interns `(trait_path, concrete_ty) → absolute_vtable_address`.
- `vtable_bytes: Vec<u8>` — the packed i32 little-endian funcref slot indices for every vtable.
- `VTABLE_BASE = 0x4000` — start of the vtable region in linear memory. The string pool grows from `STR_POOL_BASE = 8` and must not exceed `VTABLE_BASE`. Vtables grow upward; `__heap_top` is bumped past the end of vtable_bytes at end-of-codegen.

`MonoState::intern_vtable(trait_path, concrete_ty, traits, funcs)`:

1. Dedupe against existing entries.
2. `solve_impl_with_args` → `impl_idx`.
3. Walk the trait's methods; for each, find the matching FnSymbol in `funcs.entries` whose `trait_impl_idx == Some(impl_idx)` and whose path's last segment is the method name.
4. Intern a funcref-table slot for that wasm idx via `intern_table_slot` (deduped against fn-pointer slots).
5. Pack each slot as 4 little-endian bytes; record the start address (`VTABLE_BASE + vtable_bytes.len()`).

At end of `emit()`, `mono.vtable_bytes` are flushed into a Data segment at offset `VTABLE_BASE` (creating or appending to the segment). `__heap_top` is bumped past whichever data region (string pool or vtable pool) ends later.

## Mono lowering

Two new `MonoExprKind` variants:

- `RefDynCoerce { inner_ref, src_concrete_ty, trait_path }` — wraps an expression whose `dyn_coercions[expr.id]` was set. Codegen emits the inner ref's data ptr, then `i32.const <vtable_addr>`.
- `DynMethodCall { recv, method_idx, args, method_param_types, method_return_type, recv_mut, trait_path }` — emitted for MethodCall expressions whose `dyn_method_calls[expr.id]` was set.

`MonoFnInput` carries `dyn_coercions` and `dyn_method_calls` arrays, copied from `FnSymbol`/`GenericTemplate`.

## Codegen

`RefDynCoerce`: emit inner ref, then `i32.const <intern_vtable(...)>`. Two i32 scalars on the wasm stack — fat ref complete.

`DynMethodCall`:
1. (sret prefix if returning enum)
2. Emit recv expression (pushes data_ptr + vtable_ptr).
3. Cache vtable_ptr into a wasm local (LocalSet), then data_ptr into another local.
4. Push data_ptr (the `&self`/`&mut self` arg).
5. Emit each user arg.
6. Load `vtable[method_idx*4]` via `i32.load` with offset `method_idx*4`.
7. Build the FuncType (sret? + i32 for &self + per-arg flatten + per-return flatten); intern via `intern_pending_func_type`.
8. `call_indirect typeidx`.

## Open follow-ups

- Supertrait method dispatch (currently rejects with "method not found on dyn X" if the method is on a supertrait of X).
- Multi-bound `dyn A + B` (parser accepts; coercion + dispatch reject with "not supported yet").
- `Box<dyn Trait>` (Phase 3).
- `dyn Fn(T) -> R` for closures (Phase 4).
- Generic concrete types via `&dyn Trait` (e.g. `&Wrap<u32>` coercing through `impl<T> Show for Wrap<T>`) — works for non-generic impls today; generic-impl vtables need additional mono integration.
