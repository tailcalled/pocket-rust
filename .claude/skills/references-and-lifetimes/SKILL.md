---
name: references-and-lifetimes
description: Use when working with `&T` / `&mut T` references, lifetime annotations, lifetime elision, raw pointers (`*const T` / `*mut T`), `unsafe` blocks/functions, or smart-pointer deref through `Deref`/`DerefMut`. Covers reborrow patterns, `LifetimeRepr`, struct fields with lifetimes, and the borrow-vs-raw-pointer distinction.
---

# references and lifetimes

## Reference types

Shared `&T` and unique `&mut T` references are allowed in parameter types, as `&expr` / `&mut expr` expressions, and (Phase D) in struct fields with explicit lifetime annotations. Raw pointers `*const T` / `*mut T` cover the no-lifetime use cases.

## Lifetime annotations

Reference parameters and return types may carry an explicit lifetime annotation (`&'a T`, `&'a mut T`) so long as `'a` is declared in the enclosing fn/impl/struct's `<'a, ...>` lifetime params, **or it is the built-in `'static`** (always in scope without declaration; carried structurally just like any other named lifetime — Phase D doesn't enforce its "outlives everything" semantics).

Each signature ref carries a `LifetimeRepr`:
- `Named(name)` for user-written `'a`.
- `Inferred(N)` allocated per-function for elided refs.

The anonymous `'_` lifetime — both on refs (`&'_ T`) and on lifetime-generic structs (`Holder<'_>`, `impl Drop for Logger<'_>`) — is treated as a fresh `Inferred(0)` placeholder per occurrence and freshened by the same per-function pipeline as elided refs, so users don't have to invent a unique `'a` name everywhere a lifetime-generic struct appears.

## Lifetime elision

Follows Rust's rules:
- **Rule 3:** a `&self`/`&mut self` receiver wins as the lifetime source even when other ref params are present.
- **Rule 2:** otherwise, exactly one ref param's lifetime is taken as the return's.
- `&mut T -> &U` (downgrade) is allowed; `&T -> &mut U` is rejected.
- Zero or two-or-more ref params + elided ref return without a self receiver is rejected.

Refs in struct fields must use a `Named` lifetime (no elision in field types); the lifetime must be one of the struct's `<'a, ...>` params.

## Lifetime artifacts

`FnSymbol.param_lifetimes: Vec<Option<LifetimeRepr>>` and `FnSymbol.ret_lifetime: Option<LifetimeRepr>` record the resolved outermost lifetimes. Borrowck propagates a returned ref's borrows from every param whose outermost lifetime matches the return's (combined sets — `fn longer<'a>(x: &'a u32, y: &'a u32) -> &'a u32` keeps both args borrowed for the result's lifetime).

Type equality and unification currently ignore the lifetime field — it's structural carry today and will start participating in checks in a later phase.

## Field access through references

Field access through a reference is allowed only for `Copy` fields (any integer, `&U`, `&mut U`, `*const U`, `*mut U`); accessing a non-Copy struct field through a reference is rejected as "cannot move out of borrow". The same Copy rule applies to explicit deref-and-field (`(*p).field`).

Borrow conflicts (two `&mut`, or `&mut` + `&` on overlapping places) are rejected at borrowck.

## Borrows in struct fields

When a struct literal initializes a ref-typed field from a `&place` expression, the CFG produces `temp_ref = &place; outer = StructLit { field: temp_ref }`. Active-borrow propagation in `borrowck/borrows.rs` carries the `&place` borrow forward by duplicating it with `dest = outer` (gated on the outer's type containing a ref via `rtype_contains_ref`). Field reads through the wrapper (`outer.field`, `outer.inner.field`) are themselves no-ops for the borrow set — the borrow stays attached to `outer` until `outer`'s liveness ends, at which point the NLL pruning step drops it.

## Raw pointers and `unsafe`

`*const T` / `*mut T` are unrestricted compile-time citizens — they may appear in struct fields, return types, parameter types, and locals; they enable recursive types like `struct Node { next: *const Node }`.

Cast syntax `expr as Type` is the only way to produce a raw pointer:
- `&x as *const T` (and `&mut x as *mut T`) for safe-ref → raw-ptr coercion.
- `*const T as *mut T` (and vice versa) for kind switching.
- `0 as *const T` for null.

Unary `*` is the deref operator (read or `*p = …;` write); `unsafe { … }` blocks open an unsafe context.

Functions can be marked `unsafe fn name(...)` (or `pub unsafe fn`); calling an unsafe function from outside an `unsafe { … }` block is rejected by `safeck`, but inside an `unsafe fn` body all callers of further unsafe operations are implicitly covered.

The `safeck.rs` pass enforces both rules — raw-pointer deref outside unsafe and unsafe-fn-call outside unsafe — using the same `in_unsafe` boolean.

Raw pointers are Copy, carry no compile-time lifetime, and don't participate in borrow tracking (the cast-to-raw-pointer site drops the inner borrow).

## Reborrow patterns

**Implicit method-receiver reborrow:** when a method-call receiver is itself a ref (`&T` or `&mut T`) and the method takes a ref-typed self (recv_adjust = ByRef), `borrowck/build.rs::lower_recv_reborrow` lowers it as `Operand::Copy(place)` instead of the default Move. Justified semantically because the callee scope-bounds the borrow — after the call, the source binding can resume use. Without this, `&mut self` methods couldn't transitively call other `&mut self` methods on the same binding (the receiver would be Move-consumed by the inner call). Function-arg-position reborrow (passing `&mut T` to a function expecting `&mut U`) is deferred — the method-receiver case covers Vec's needs.

**Implicit deref reborrow:** when `*expr` is lowered to a place via `lower_expr_place`'s `Deref` arm, the inner ref expression is read as a place (not consumed) when it's a place-form expression (`Var` / `FieldAccess` / `TupleIndex` / nested `Deref`). This is what lets `*self = ¤u8_add(*self, other);` typecheck even though `&mut T` is non-Copy — both `*self` reads use `self` without recording a move. Non-place inners (`*foo()`) still go through the materialization path, which is appropriate because the temp holds the only copy of the ref.

**Raw-pointer deref doesn't track moves:** when an `Operand::Move(place)` has a `Deref` projection rooted at a `*const T` / `*mut T` local, `apply_operand` skips `state.mark(place, Moved)` (`is_through_raw_ptr_deref`). Raw pointers don't carry compile-time ownership, so `let v = unsafe { *raw };` followed by `raw.cast::<U>()` is a sound reading-from-pointer pattern that the place-overlap check would otherwise reject. `Box::into_inner` and similar raw-ptr-extraction patterns rely on this.

## Smart-pointer deref via `Deref` / `DerefMut`

`*x` for `x` of a non-builtin-ref/ptr type routes through `<X as Deref>::deref(&x)` (read) or `<X as DerefMut>::deref_mut(&mut x)` (write). Typeck (`check_deref` in value position, `check_place_inner`'s `Deref` arm in place position, `check_deref_rooted_assign` for `*x = …;`) looks up the impl's `Target` binding via `find_assoc_binding(traits, x_type, Deref, "Target")` and uses that as the result type.

Codegen (`codegen_deref_via_trait` for reads, the smart-pointer branch of `codegen_deref_assign` for writes) resolves the impl via `solve_impl`, monomorphizes the method if generic, pushes `&x` / `&mut x` as the recv, and calls the trait method — the result is an `i32` address from which the Target leaves are loaded (or stored to). `Box<T>` is the canonical user; future smart pointers (`Rc`, `Arc`, `RefCell` borrow guards) will plug in here.

## `&*ptr` codegen note

`&*ptr` / `&mut *ptr` is a place borrow — codegen evaluates `ptr` (pushes its i32 address) and uses that directly as the borrow's value, with no fresh shadow-stack slot. Without this, `&mut *raw_ptr` would copy the pointee through a temp, and writes through the resulting `&mut T` would target the temp instead of the raw pointer's destination — breaking idioms like `Vec::get_mut` that turn a computed `*mut T` back into a `&mut T`.
