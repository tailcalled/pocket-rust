// Inherent methods on the slice DST `[T]`. Mirrors the surface of
// Rust's `std::primitive::slice` module. Methods take `&self` (for
// reads) or `&mut self` (for writes); the receiver lowers as the
// 2-i32 fat ref (data ptr + length). Construction of a slice ref
// happens via `Vec::as_slice` / `as_mut_slice` (and a future
// `slice::from_raw_parts` equivalent) — methods here only consume
// an existing slice ref.

use crate::mem;
use crate::option::Option;

impl<T> [T] {
    // The number of elements in the slice. Reads the length half of
    // the fat ref — a pure metadata access, no memory load.
    pub fn len(&self) -> usize {
        ¤slice_len::<T>(self)
    }

    // True iff the slice contains zero elements.
    pub fn is_empty(&self) -> bool {
        ¤slice_len::<T>(self) == 0
    }

    // Pointer to the slice's data buffer (start of element 0). Reads
    // the data-ptr half of the fat ref — a pure metadata access, no
    // memory load.
    pub fn as_ptr(&self) -> *const T {
        ¤slice_ptr::<T>(self)
    }

    // Borrow element `idx` if in bounds, else `None`. Computes the
    // element address as `as_ptr() + idx * size_of::<T>()` and
    // re-borrows it as `&T`. Always safe — the bounds check protects
    // the dereference; out-of-range indexes return `None` rather than
    // wrapping around.
    pub fn get(&self, idx: usize) -> Option<&T> {
        if idx >= ¤slice_len::<T>(self) {
            Option::None
        } else {
            let offset: usize = idx * mem::size_of::<T>();
            unsafe {
                let base: *const T = ¤slice_ptr::<T>(self);
                let elt: *const T = base.cast::<u8>().byte_add(offset).cast::<T>();
                Option::Some(&*elt)
            }
        }
    }

    // Mutable counterpart of `as_ptr`.
    pub fn as_mut_ptr(&mut self) -> *mut T {
        ¤slice_mut_ptr::<T>(self)
    }

    // Mutable counterpart of `get`. Returns `Some(&mut T)` for an
    // in-bounds index, `None` otherwise. The exclusive borrow on the
    // returned reference is tied to `&mut self`'s borrow of the
    // slice.
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        if idx >= ¤slice_len::<T>(self) {
            Option::None
        } else {
            let offset: usize = idx * mem::size_of::<T>();
            unsafe {
                let base: *mut T = ¤slice_mut_ptr::<T>(self);
                let elt: *mut T = base.cast::<u8>().byte_add(offset).cast::<T>();
                Option::Some(&mut *elt)
            }
        }
    }
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically.
//
// TODO: chunks(&self, size) / chunks_exact / chunks_mut — needs iterator traits.
// TODO: contains(&self, x: &T) — needs `T: PartialEq` and a loop.
// TODO: first(&self) -> Option<&T> / first_mut / last / last_mut — trivial wrappers around `get(0)` / `get(len-1)`; add when there's a caller.
// TODO: iter(&self) / iter_mut(&mut self) — needs iterator traits.
// TODO: reverse(&mut self) — needs in-place swap of T values via `&mut [T]`.
// TODO: sort / sort_by / sort_unstable* — needs `T: Ord` plus a sort algorithm.
// TODO: split_at(&self, mid) -> (&[T], &[T]) / split_at_mut — needs returning a tuple of slices, which means tuple-of-fat-ref ABI in codegen.
// TODO: starts_with(&self, needle: &[T]) / ends_with — needs `T: PartialEq` plus per-element comparison.
// TODO: swap(&mut self, a: usize, b: usize) — needs in-place index-based swap; expressible today over `get_mut`, add when there's a caller.
// TODO: windows(&self, size) — needs iterator traits.
