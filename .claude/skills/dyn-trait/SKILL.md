---
name: dyn-trait
description: Use when working with trait objects `&dyn Trait` / `&mut dyn Trait` / `Box<dyn Trait>` / `dyn Fn(T) -> R` — the parser/AST/RType plumbing, lazy object-safety check, coercion mechanics, vtable storage (drop slot + method slots) in the data segment, `DynMethodCall` dispatch through `call_indirect`, codegen-driven Drop path for `Box<dyn>`, and Fn-family closure dispatch. Phases 2-4 of the dyn-trait roadmap. Phase 1 (`fn-pointers`) is the foundation.
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
- `noop_drop_wasm_idx: Option<u32>` — wasm index of a synthesized `(param i32) -> ()` no-op fn, pre-allocated at the start of every `emit()`. Used as the drop slot for vtables of non-Drop concrete types.

### Vtable layout

Each vtable starts with a **drop fn slot at offset 0**, followed by one i32 slot per declared trait method in declaration order. Method index in `DynMethodCall` is the position in the trait's `methods` list; codegen reads `vtable[(method_idx + 1) * 4]` to skip the drop header.

```
offset 0:  drop_fn      ← funcref-table slot for the concrete type's drop
offset 4:  method[0]    ← first trait method
offset 8:  method[1]
...
```

For Drop concrete types, the drop slot points at `<T as Drop>::drop`. For non-Drop types it points at the synthesized no-op fn.

### `MonoState::intern_vtable(trait_path, concrete_ty, traits, funcs)`

1. Dedupe against existing entries.
2. `solve_impl_with_args` → `impl_idx`.
3. **Drop slot**: lookup `impl Drop for concrete_ty` via `solve_impl_with_args(drop_trait_path(), …)`. If found, intern the impl's `drop` fn; else fall back to `noop_drop_wasm_idx`.
4. Walk the trait's methods; for each, find the matching FnSymbol in `funcs.entries` whose `trait_impl_idx == Some(impl_idx)` and whose path's last segment is the method name. Intern its funcref-table slot.
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

## `Box<dyn Trait>` (Phase 3)

Owned trait objects build on Phase 2's machinery plus three additions:

### Fat raw pointers

`*mut/const dyn Trait`, `*mut/const [T]`, `*mut/const str` flatten to **2 i32s** (data ptr + len/vtable). Updated in `flatten_rtype` / `byte_size_of` / `rtype_size` / `collect_leaves`. This makes `Box<DST>` automatically fat: `Box<T>` has body `ptr: *mut T`, so substituting T = Dyn yields `*mut dyn Trait` — a fat raw ptr — and the surrounding struct flattens to its 2-i32 contents.

### `Box<T>` → `Box<dyn Trait>` coercion

`coerce_at` recognizes the shape `Struct{["std","boxed","Box"], [T]}` → `Struct{["std","boxed","Box"], [Dyn{...}]}` and runs the same object-safety + impl-existence checks as the ref case. Records `DynCoercion { kind: BoxOwned, src_concrete_ty: T, trait_paths }`.

**Today's limitation:** typeck propagates the let-anno target into a `Box::new(...)` call's type-arg inference, so writing `let b: Box<dyn Show> = Box::new(Foo { v: 42 })` is rejected (the call's arg slot is then expected to be `dyn Show`). Workaround: bind as `Box<T>` first, then coerce:

```
let bf: Box<Foo> = Box::new(Foo { v: 42 });
let b: Box<dyn Show> = bf;          // dyn coercion fires here
```

### Method dispatch on `Box<dyn Trait>`

`check_method_call` recognizes `recv: Box<Dyn>` and routes to `check_dyn_method_call(..., recv_mut: true)` (the box owns its T, so any receiver shape works). Codegen extracts the box's two i32s (data ptr + vtable ptr) as the fat receiver — same emission path as `&dyn Trait`.

Borrowck derives `recv_adjust = BorrowMut` for `Box<dyn>` recv (from the per-NodeId `dyn_method_calls` artifact), so `box.method()` doesn't move the box.

### Drop for `Box<dyn Trait>`

The user-written `impl<T> Drop for Box<T>` body has `let v: T = unsafe { *self.ptr };` which assumes sized T — invalid for T = dyn. Two pieces handle this:

1. `mono::register_drop_mono` skips Box<dyn _> when registering drop monomorphizations.
2. `emit_drop_walker` short-circuits Box<dyn _>: load data_ptr (offset 0), load vtable_ptr (offset 4), load drop fn slot from `vtable[0]`, `call_indirect` it with data_ptr, then `¤free(data_ptr)` (no-op stub today).

The drop slot of vtables for non-Drop concrete types points at the synthesized no-op fn (`MonoState::noop_drop_wasm_idx`).

## `dyn Fn(T) -> R` for closures (Phase 4)

Trait objects for the Fn-family traits (`Fn` / `FnMut` / `FnOnce`) — `&dyn Fn(u32) -> u32`, `Box<dyn FnMut(...)>`, etc. — built on Phase 2-3 plus four additions:

### `RType::Dyn` carries trait_args + assoc_bindings

`RType::Dyn` and `InferType::Dyn` now hold `Vec<DynBound>` / `Vec<InferDynBound>` instead of bare `Vec<Vec<String>>`. Each bound is:

```
struct DynBound {
    trait_path: Vec<String>,
    trait_args: Vec<RType>,           // `Fn<(u32,)>` → [Tuple([u32])]
    assoc_bindings: Vec<(String, RType)>, // `Output = R` → [("Output", R)]
}
```

`resolve_type`'s Dyn arm captures both from the `parse_trait_bound` output. Equality (`dyn_bounds_eq` / `infer_dyn_bounds_eq`) compares all three components. `DynCoercion` records carry the full bounds (not just trait paths) so codegen has the trait_args when building vtables.

### Object-safety relaxations

Two changes in `dyn_safety::check_object_safety`:

1. **`AssocProj` of `Self` is OK in non-receiver positions.** `fn call(&self, args) -> Self::Output` no longer fails — the dyn type's `assoc_bindings` make the concrete output known per-impl. (The check still rejects bare `Self` in args/return.)
2. **Supertrait methods aren't required to be object-safe.** `dyn Fn`'s vtable only carries `Fn::call`; `FnOnce::call_once`'s by-value receiver doesn't matter because it's not in the vtable. The supertrait closure walk was removed entirely.

### Substituting method signatures at dispatch

`check_dyn_method_call` and the new `check_bare_dyn_fn_call` resolve the method's `Self::AssocName` projections via the dyn type's `assoc_bindings`, and the trait's positional type-params via `trait_args`:

- `substitute_self_assoc(t, assoc_bindings)` walks an RType replacing `<Self as ?>::Name` with the binding's value when the base is `Self`.
- `substitute_trait_args(t, trait_type_params, trait_args)` substitutes via the standard `substitute_rtype` env.

Applied to `Fn::call`'s `(&Self, Args) -> Self::Output` signature: `Args` substitutes to the recorded tuple, `Self::Output` substitutes to the recorded R. The result feeds `DynMethodDispatch.method_param_types` / `method_return_type`, which codegen reads to build the `call_indirect` typeidx.

### Bare-call sugar for `f(args)` through `&dyn Fn`

`check_call`'s "local-shadows-fn" branch detects `f: &dyn Fn(...)` / `&mut dyn FnMut(...)` / `Box<dyn Fn>` and routes to `check_bare_dyn_fn_call`, which records both:

- `dyn_method_calls[expr_id]` — the dispatch info (trait_path, method_idx, substituted method signature).
- `bare_closure_calls[expr_id]` — the callee binding name (mono uses this to find the receiver).

Mono's `ExprKind::Call` handler checks `dyn_method_calls` first; when set, it lowers the call as `MonoExprKind::DynMethodCall { recv: Local(binding), args: vec![Tuple(c.args)], ... }`. The args get packed into a tuple to match the `call(&self, args: Args)` shape.

### Closure-impl exemption at coercion

Synthesized closure structs (`__closure_N`) have their `Fn`/`FnMut`/`FnOnce` impls registered by `closure_lower` after typeck, so `solve_impl` returns None at typeck-coercion time. `coerce_at` exempts closure srcs from the impl-existence check for both `&T → &dyn Trait` and `Box<T> → Box<dyn Trait>` shapes; the impl is verified later at codegen.

### Vtable building with trait_args

`MonoExprKind::RefDynCoerce` now carries `trait_args`; `MonoState::intern_vtable` accepts `trait_args` and passes them to `solve_impl_with_args` so generic Fn impls (`impl<Args> Fn<Args> for __closure_N`) match correctly.

## Multi-bound `dyn A + B` (Phase 5)

Vtable layout extends to: `[drop, A's method slots..., B's method slots...]`. `intern_vtable` walks each bound in declaration order, packing all its method slots. `check_dyn_method_call` (and `intern_vtable`) compute method indexes within the contiguous post-drop region; the absolute index used by `DynMethodCall` is `bound_slot_base + offset_within_bound`.

Method-name ambiguity (the same method declared by two principals) is rejected at dispatch with "ambiguous method `X` on multi-bound dyn type" — UFCS disambiguation isn't supported.

Each bound is independently object-safety-checked at coercion.

## Supertrait method dispatch (Phase 6)

`dyn_safety::dyn_vtable_methods(trait_path, trait_args, traits)` walks a trait + its transitive supertrait closure (BFS), returning `(declaring_trait_path, method_idx, trait_args)` triples in vtable order: direct methods first, then supertrait methods. Object-unsafe supertrait methods are silently **skipped** — they don't fail obj-safety, just don't appear in the vtable (mirrors Rust's `where Self: Sized` carve-out without requiring that syntax). This lets `dyn Fn` keep working: `FnOnce::call_once` (by-value, unsafe) drops out, while `FnMut::call_mut` (safe) stays.

Supertrait trait_args are substituted through the parent trait's type-params: `trait Fn<Args>: FnMut<Args>` → the dyn type's `Fn<(u32,)>` becomes `FnMut<(u32,)>` for the FnMut slots.

Method dispatch on `&dyn Show` where `trait Show: Tag` finds `tag` via the supertrait closure (each `(declaring_trait, method_idx)` triple is searched), then dispatches through the slot for the right supertrait's impl.

`intern_vtable` uses the same walker, resolving each declaring-trait via its own `solve_impl_with_args` row.

## Generic-impl vtables (Phase 7)

`&Wrap<u32>` coercing through `impl<T> Show for Wrap<T>` works because `intern_vtable`'s impl-method lookup falls back to `funcs.templates[]` when the non-generic `funcs.entries[]` search misses. For each generic-impl method:

1. Find the template with `trait_impl_idx == Some(impl_idx)` and matching method name.
2. Build concrete type-args from `solved.subst` (the impl-level type-param bindings).
3. Call `mono_table.intern(template_idx, concrete)` — lazily monomorphize and get the slot's wasm idx.

The monomorphization expansion happens in the post-emit mono loop (the same loop that handles regular generic call sites).

## `Box<dyn Trait>` direct coercion (Phase 8)

`let b: Box<dyn Show> = Box::new(Foo { v: 42 })` works directly. `check_let_stmt` type-checks the value expression *without* the annotation's type-arg hint pre-pinning Box::new's `T = dyn Show`; `T` infers to `Foo` from the argument, then `coerce_at` runs at the let boundary and converts `Box<Foo> → Box<dyn Show>`.

## Identity Ref→Dyn fall-through (Phase 9 adjacent)

`coerce_at` guards against firing the `&T → &dyn Trait` coercion when `T` is already `Dyn`. Without the guard, passing `s: &dyn Show` to a fn parameter of type `&dyn Show` would retrigger obj-safety + impl-existence on the source — which fails vacuously since `dyn Show` doesn't `impl Show`. The guard delegates to plain `unify` instead.

Path-equivalence beyond this: trait paths from `resolve_type` are canonicalized at every dispatch + coercion site via `canonicalize_dyn_trait_path`, which uses the full `resolve_trait_path` (TraitTable-aware, sees globs + re-exports). Different `use` spellings for the same trait still produce the same canonical form at dispatch.

## Open follow-ups

- `dyn FnOnce(...)` — the by-value `call_once(self)` needs the `Box<Self>` receiver exemption (Rust's `where Self: Sized` carve-out). `dyn Fn` and `dyn FnMut` work today.
- Method-level type-params on dyn dispatch (obj-safety rejects them today; a future generalization could allow them as long as they don't appear in the vtable signature).
- `dyn Trait` equivalence across distinct compilation contexts (multi-module path normalization beyond the use-scope canonicalization done at use sites).
