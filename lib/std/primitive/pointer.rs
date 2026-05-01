// Inherent methods on raw pointers `*const T` and `*mut T`. Mirrors
// the surface of Rust's `std::primitive::pointer` module.
//
// Pointer arithmetic in pocket-rust is always **byte-wise** at this
// layer — `byte_add(self, n)` advances `self` by `n` raw bytes. The
// `count`-scaled `add(self, n)` etc. need a `size_of::<T>()` intrinsic
// that doesn't exist yet (see TODO list at the bottom of this file).
//
// `byte_add` / `byte_sub` / `byte_offset` are `unsafe fn` — the
// resulting pointer's address is unverified and dereferencing it (or
// passing it to another routine that does) can produce out-of-bounds
// reads/writes. `is_null` is safe — it just compares the address to 0.

impl<T> *const T {
    pub unsafe fn byte_add(self, count: usize) -> *const T {
        ¤ptr_usize_add(self, count)
    }

    pub unsafe fn byte_sub(self, count: usize) -> *const T {
        ¤ptr_usize_sub(self, count)
    }

    pub unsafe fn byte_offset(self, count: isize) -> *const T {
        ¤ptr_isize_offset(self, count)
    }

    // True iff `self` is the null pointer (address 0). Always safe.
    pub fn is_null(self) -> bool {
        let addr: usize = self as usize;
        addr == 0
    }

    // Reinterpret the pointer as `*const U` (preserving its const-ness
    // and address). Always safe — only dereferencing the resulting
    // pointer is unsafe. Wraps the `¤cast` intrinsic, which is a
    // codegen no-op (raw pointers all flatten to a wasm `i32`).
    pub fn cast<U>(self) -> *const U {
        ¤cast::<U, T>(self)
    }
}

impl<T> *mut T {
    pub unsafe fn byte_add(self, count: usize) -> *mut T {
        ¤ptr_usize_add(self, count)
    }

    pub unsafe fn byte_sub(self, count: usize) -> *mut T {
        ¤ptr_usize_sub(self, count)
    }

    pub unsafe fn byte_offset(self, count: isize) -> *mut T {
        ¤ptr_isize_offset(self, count)
    }

    pub fn is_null(self) -> bool {
        let addr: usize = self as usize;
        addr == 0
    }

    pub fn cast<U>(self) -> *mut U {
        ¤cast::<U, T>(self)
    }
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically.
//
// TODO: add(self, count) / sub(self, count) / offset(self, count) — needs a `size_of::<T>()` intrinsic so the count gets scaled by the pointee size; today only the byte-wise forms are available.
// TODO: addr(self) — would just be `self as usize`; semantically distinct in Rust (provenance handling) but pocket-rust has no provenance model.
// TODO: align_offset(self, align) — needs the alignment math + a non-trivial generic body that works for arbitrary T.
// TODO: as_ref(self) -> Option<&T> / as_mut(self) -> Option<&mut T> — needs an unsafe-flavoured cast from `*const T` to `&T` plus null-checking; the `&T`/`&mut T` flavours have lifetime concerns we haven't tackled.
// TODO: copy_from(dst, src, count) / copy_from_nonoverlapping — needs a memcpy intrinsic exposed at the language level.
// TODO: guaranteed_eq(self, other) / guaranteed_ne — needs `bool` returns from `==` on raw pointers, which in turn needs a `PartialEq` impl on raw pointers.
// TODO: is_aligned(self) / is_aligned_to(self, align) — needs alignment math and `size_of` / `align_of` intrinsics.
// TODO: offset_from(self, origin) -> isize — needs signed pointer subtraction (a `¤ptr_diff_isize` intrinsic).
// TODO: read(self) / write(self, val) / replace(self, val) — currently expressible only via the `unsafe { *p }` / `unsafe { *p = … }` syntax; method versions need to thread `unsafe` through the call site.
// TODO: read_unaligned / write_unaligned — needs unaligned load/store intrinsics (wasm has these natively).
// TODO: with_addr(self, addr) — needs the `addr` story plus a way to construct a pointer from a `usize` without going through `as`.
// TODO: wrapping_add / wrapping_sub / wrapping_offset / wrapping_byte_add / wrapping_byte_sub / wrapping_byte_offset — same shape as the non-wrapping forms but with explicit overflow semantics; today's i32.add/i32.sub already wrap, so the wrapping_byte_* forms could be aliases — leaving as TODO until there's a reason to distinguish.
