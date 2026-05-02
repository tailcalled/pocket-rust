---
name: stdlib-layout
description: Use when navigating or modifying `lib/std/` — pocket-rust's own in-language standard library. Lists every module, what types/traits/methods it provides, and the relationships between them.
---

# `lib/std/` — pocket-rust's own standard library

**Not referenced from `src/`.** It's a regular directory of `.rs` files that the host (`main.rs` and the test helpers) loads from disk and hands to `compile` as one of its `libraries`.

## `lib/std/lib.rs` — crate root

Declares submodules and re-exports the canonical types:
- `Copy`, `Drop`, `Index`, `IndexMut`, `Deref`, `DerefMut`
- `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `Not`
- `AddAssign`, `SubAssign`, `MulAssign`, `DivAssign`, `RemAssign`
- `PartialEq`, `Eq`, `PartialOrd`, `Ord`
- `Option`, `Result`, `Vec`, `Box`

## `lib/std/marker.rs`

Defines `Copy` (pure marker trait) and primitive `impl Copy for {u8, i8, …, isize, bool, char} {}`, `impl<T> Copy for &T {}`, `impl<T> Copy for *const T {}`, `impl<T> Copy for *mut T {}`.

## `lib/std/ops.rs`

Defines:
- `Drop { fn drop(&mut self); }` — destructor.
- `Index<Idx> { type Output; fn index(&self, idx: Idx) -> &Self::Output; }`.
- `IndexMut<Idx>: Index<Idx> { fn index_mut(&mut self, idx: Idx) -> &mut Self::Output; }`.
- Range types (no methods, plain data): `Range<Idx> { start, end }`, `RangeFrom<Idx> { start }`, `RangeTo<Idx> { end }`, `RangeInclusive<Idx> { start, end }`, `RangeToInclusive<Idx> { end }`, `RangeFull` (unit struct). Constructed by the parser's range-literal desugar.
- `Add<Rhs = Self>` / `Sub<Rhs = Self>` / `Mul<Rhs = Self>` / `Div<Rhs = Self>` / `Rem<Rhs = Self>` — Rust-style operator-overloading traits, each with `type Output; fn op(self, other: Rhs) -> Self::Output;`.
- `Neg { type Output; fn neg(self) -> Self::Output; }`.
- `Not { type Output; fn not(self) -> Self::Output; }`.
- `AddAssign<Rhs = Self>` / `SubAssign<Rhs = Self>` / `MulAssign<Rhs = Self>` / `DivAssign<Rhs = Self>` / `RemAssign<Rhs = Self>` — compound-assignment, each `fn op_assign(&mut self, other: Rhs);`.

Every primitive integer kind has same-Self impls of all six binary ops + Neg + all five `*Assign`s. Cross-kind arithmetic requires an explicit `as` cast first. `Not` impls only exist for `bool`; integer-bitwise-NOT impls are TODO.

## `lib/std/cmp.rs`

Comparison traits + primitive impls:
- `PartialEq { fn eq(&self, other: &Self) -> bool; fn ne(&self, other: &Self) -> bool { … } }`.
- `Eq: PartialEq {}` — pure marker.
- `PartialOrd { fn lt/le/gt/ge(&self, other: &Self) -> bool; }` (and `partial_cmp`).
- `Ord: PartialOrd + Eq {}` — pure marker.

Every primitive provides `impl PartialEq + impl Eq + impl PartialOrd + impl Ord`, with PartialEq/PartialOrd carrying the actual method bodies.

## `lib/std/mem.rs`

- `pub fn drop<T>(_x: T) {}` — mirrors `std::mem::drop`. Consumes T by value so the existing scope-end Drop machinery runs `T::drop` for Drop types and is a no-op for non-Drop types.
- `pub fn size_of<T>() -> usize` — thin wrapper around `¤size_of::<T>()`.

## `lib/std/option.rs`

`Option<T>` (`None` / `Some(T)`). Methods: `is_some`, `is_none`, `unwrap_or`, `and`, `or`, `xor`, `flatten`.

## `lib/std/result.rs`

`Result<T, E>` (`Ok(T)` / `Err(E)`). Methods: `is_ok`, `is_err`, `unwrap_or`, `ok`, `err`, `and`, `or`. Second impl blocks: `flatten` on `Result<Result<T, E>, E>` and `transpose` on `Result<Option<T>, E>`.

## `lib/std/vec.rs`

`Vec<T>` — heap-backed dynamic array; bump-grow capacity 0→4→8→…

Methods: `new`, `len`, `is_empty`, `capacity`, `push`, `pop`, `get`, `get_mut`, `clear`.

Drop impl calls `mem::drop` on each element then `¤free`s the buffer. Uses the `mem::size_of` / `*mut T::byte_add` / `*mut T::cast` stdlib wrappers internally rather than the underlying intrinsics, so the only intrinsics it touches directly are `¤alloc` and `¤free` (which have no wrappers yet).

`Index<usize>` / `IndexMut<usize>` impls cover element indexing. Range slicing impls (`Index<Range<usize>>` / `Index<RangeFrom<usize>>` / `Index<RangeTo<usize>>` / `Index<RangeInclusive<usize>>` / `Index<RangeToInclusive<usize>>` / `Index<RangeFull>`, plus matching `IndexMut`) wrap the corresponding `[T]` slicing impls via `as_slice` / `as_mut_slice`. All do bounds checks via `panic!`.

## `lib/std/boxed.rs`

`Box<T>` — heap-allocated single-value smart pointer.

Methods: `new`, `into_raw`, `from_raw`, `into_inner`, `as_ptr`, `as_mut_ptr`, `leak`. Plus `Deref` / `DerefMut` / `Drop` impls.

Drop runs T's destructor (if Drop) and frees the buffer; `into_raw` / `into_inner` / `leak` use a null-ptr sentinel in the buffer-pointer field to suppress the Drop impl when ownership is handed off.

## `lib/std/primitive.rs` — re-export hub for primitive-type methods

Currently re-exports:
- `lib/std/primitive/pointer.rs` — inherent `unsafe fn byte_add` / `byte_sub` / `byte_offset`, safe `is_null`, and safe `cast::<U>(self)` on `*const T` and `*mut T`.
- `lib/std/primitive/slice.rs` — `[T]` methods (`len`, `is_empty`, `as_ptr`, `as_mut_ptr`, `get`, `get_mut`); `Index`/`IndexMut` impls for `Idx = usize` with bounds checks via `panic!`.
- `lib/std/primitive/str.rs` — `str` methods (`len`, `is_empty`, `as_bytes`, `is_char_boundary`); `Index<Range*<usize>>` / `IndexMut<Range*<usize>>` impls covering all six range forms. Slicing impls bounds-check the byte indices AND char-boundary-check them via `is_char_boundary` (which tests for UTF-8 continuation bytes via the `0xC0` mask) — slicing mid-codepoint panics rather than producing an invalid `&str`. Mutable slicing constructs `&mut str` via the new `¤make_mut_str` / `¤str_as_mut_bytes` builtins (mirrors of the existing immutable variants; codegen is identical pass-through).

## `lib/std/dummy.rs`

Placeholder.

## Library system invariants

- The `Library` struct carries `prelude: bool`. For `std`, it's `true` — `compile` injects `use std::*;` at every other crate's root before typeck.
- The library's items live at `["std", ...]`; the user crate's at the empty prefix. The "export iff `current_module.is_empty()`" rule in codegen exports user crate-root functions and never library functions.
- Errors in library code are attributed to the file paths the library's VFS was populated with (e.g. `lib.rs`, `dummy.rs`), not synthetic `<std>/...` paths.

## Adding new types/traits — the parity rule

When adding a new type or trait under `lib/std/`, walk the corresponding Rust standard-library API and explicitly account for every method:
- Methods you can express in pocket-rust today: implement them.
- Methods you have to skip: leave a `// TODO: <method-name> — <reason>` comment in the same file (typically grouped at the bottom of the relevant `impl` block, alphabetized so future readers can scan). The reason should name the missing language feature (closures, `!`/never type, `Result`, `Default`, `mem::replace`, iterators, strings, etc.) so a `grep -r "TODO" lib/std/` finds everything that becomes implementable when a given feature lands.

The point is to keep the gap between pocket-rust's stdlib and Rust's visible.
