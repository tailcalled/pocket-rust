---
name: builtin-intrinsics
description: Use when working with the `¤` builtin intrinsic family — arithmetic/comparison ops, heap allocation, pointer cast/arithmetic, type size, slice/str construction. Catalog of every recognized intrinsic, its signature, and how it lowers at codegen.
---

# `¤` builtin intrinsics

Syntax: `¤<name>(args)` or `¤<name>::<TypeArgs...>(args)` (the currency sign `¤` U+00A4, lexed as `TokenKind::Builtin`; turbofish parsed by `parse_builtin` when followed by `::<…>`, dispatched by `check_builtin` and `codegen_builtin`).

## Arithmetic / comparison

`¤<type>_<op>(args)`. `<type>` is `bool` or any int kind (`u8`/`i8`/.../`u64`/`i64`/`u128`/`i128`/`usize`/`isize`).

`<op>`:
- `add`/`sub`/`mul`/`div`/`rem`/`and`/`or`/`xor` — arithmetic, signature `(T,T) -> T`.
- `eq`/`ne`/`lt`/`le`/`gt`/`ge` — comparison, signature `(T,T) -> bool`.
- `not` — bool only, `(bool) -> bool`.

`builtin_signature` decides arity + types from the name; `codegen_builtin` lowers to a single wasm instruction for ≤64-bit ops (signed/unsigned variant chosen from the int kind's signedness for `div`/`rem`/`lt`/`le`/`gt`/`ge`; `bool_not` lowers to `i32.eqz`).

128-bit ops dispatch into `codegen_builtin_128`, which pops the four i64 args into locals and emits a multi-instruction sequence:
- `add`/`sub` use carry/borrow detection (`i64.lt_u` against the original low half).
- `eq`/`ne` combine per-half `i64.eq` with `i32.and`.
- `lt`/`le`/`gt`/`ge` decompose into `(high OP_high high) || (high == high && low op_low_unsigned low)` with the high-half op picked signed for i128 / unsigned for u128.
- `mul`/`div`/`rem` on 128-bit emit `unreachable` (runtime trap) — bootstrap doesn't need them.

## Heap allocation

`¤alloc(n: usize) -> *mut u8` and `¤free(p: *mut u8)`.

The heap lives in linear memory above the null-territory bytes (offsets 0..7) and below the shadow stack; `__heap_top` is a mutable i32 global at index 1, initialized to 8 (or higher when string-pool data is baked in).

`alloc` is a pure bump allocator: stash `__heap_top` into a wasm-local result, advance `__heap_top += n`, return the stashed value.

`free` is currently a **no-op stub** — it evaluates and discards its argument, leaking the allocation. Allocations are leaked, and the heap collides silently with the shadow stack if either grows too far. Future work: real free-list / arena allocator (the `¤free` intrinsic is the hook point so user-side code doesn't need to change when it lands).

## Pointer-type cast

`¤cast::<A, B>(p) -> *X A` where `p: *X B` (X = `const` or `mut`, preserved). Turbofish args are mandatory; type inference is not used (typeck rejects bare `¤cast(p)` with an explicit "missing type argument" error).

The receiver must already be a raw pointer — no auto-coercion from `&T`/`&mut T`/integers (use `expr as *mut T` for those).

The operation is a pure no-op at codegen time: raw pointers flatten to one i32 regardless of pointee type, so the wasm value passes through unchanged. The intrinsic exists because `as` is awkward when the source pointee type isn't already a single named type; `¤cast` makes both source and destination pointees explicit.

## Pointer arithmetic

- `¤ptr_usize_add(p, n: usize) -> *X T`
- `¤ptr_usize_sub(p, n: usize) -> *X T`
- `¤ptr_isize_offset(p, n: isize) -> *X T`

**Byte-wise** offsets (no `size_of::<T>()` scaling) — the inputs' mutability and pointee type are preserved on the result. Each lowers to a single `i32.add` / `i32.sub`; signed offsets use the same unsigned add/sub since wasm's i32 ops are sign-agnostic two's-complement.

The stdlib's `*const T::byte_add(self, n)` / `byte_sub` / `byte_offset` / `is_null` (in `lib/std/primitive/pointer.rs`) wrap these directly. The `*T as <int>` cast is also accepted (typeck allows raw-pointer source for integer-target casts; codegen treats it like `usize as <target>` since both flatten to i32).

## Type size

`¤size_of::<T>() -> usize`. Mandatory turbofish (no inference); typeck rejects bare `¤size_of()` with "takes 1 type argument".

The resolved `T` is recorded per-`Expr.id` on `FnSymbol.builtin_type_targets: Vec<Option<Vec<RType>>>` (a generic per-Builtin artifact — currently only `size_of` populates it; future intrinsics that need their resolved type-args at codegen can reuse the slot). Codegen substitutes T through the mono env, computes `byte_size_of(T, structs, enums)`, and emits a single `i32.const`.

Used by `Vec<T>` and any other code that needs to compute element offsets without a `size_of` Rust-shaped wrapper.

## Slice construction / inspection

- `¤make_slice::<T>(ptr: *const u8, len: usize) -> &[T]` — constructs fat ref from raw parts.
- `¤make_mut_slice::<T>(ptr: *mut u8, len: usize) -> &mut [T]` — same, mutable.

Codegen for both is a pure no-op: both args already flatten to one i32, leaving (ptr, len) on the wasm stack — which is exactly the fat-ref shape.

- `¤slice_len::<T>(s)` — returns the length half (drops the ptr leaf, keeps the len leaf); accepts either `&[T]` or `&mut [T]` since the read is identical regardless of mutability.
- `¤slice_ptr::<T>(s: &[T]) -> *const T` — returns the data-ptr half.
- `¤slice_mut_ptr::<T>(s: &mut [T]) -> *mut T` — returns the mutable data-ptr half.

`Vec<T>::as_slice` / `as_mut_slice` use `make_slice` / `make_mut_slice`; `[T]::len` / `is_empty` use `slice_len`; `[T]::as_ptr` / `as_mut_ptr` / `get` / `get_mut` use `slice_ptr` / `slice_mut_ptr` (with bounds-checked `byte_add` + `cast` for indexed access).

## `str` construction / inspection

- `¤make_str(ptr: *const u8, len: usize) -> &str` — raw-parts route, identical codegen to `¤make_slice` since the ABIs match.
- `¤str_len(s: &str) -> usize` — reuses `¤slice_len`'s codegen.
- `¤str_as_bytes(s: &str) -> &[u8]` — 1-arg pure pass-through; `&str` and `&[u8]` share the fat-ref shape.

`str::len` / `is_empty` / `as_bytes` (in `lib/std/primitive/str.rs`) wrap these.
