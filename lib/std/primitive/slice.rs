// Inherent methods on the slice DST `[T]`. Mirrors the surface of
// Rust's `std::primitive::slice` module. Methods take `&self` (for
// reads) or `&mut self` (for writes); the receiver lowers as the
// 2-i32 fat ref (data ptr + length). Construction of a slice ref
// happens via `Vec::as_slice` / `as_mut_slice` (and a future
// `slice::from_raw_parts` equivalent) — methods here only consume
// an existing slice ref.

use crate::mem;
use crate::option::Option;
use crate::ops::Index;
use crate::ops::IndexMut;
use crate::ops::Range;
use crate::ops::RangeFrom;
use crate::ops::RangeTo;
use crate::ops::RangeInclusive;
use crate::ops::RangeToInclusive;
use crate::ops::RangeFull;

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

// `arr[idx]` desugar — Index/IndexMut for `[T]` with `Idx = usize`.
// Bounds-checked: on out-of-range index, calls `panic!` which
// invokes the host-imported `env.panic`. Returns `&T` / `&mut T` to
// the element in place.
impl<T> Index<usize> for [T] {
    type Output = T;
    fn index(&self, idx: usize) -> &T {
        if idx >= ¤slice_len::<T>(self) {
            panic!("slice index out of bounds")
        }
        let offset: usize = idx * mem::size_of::<T>();
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            let elt: *const T = base.cast::<u8>().byte_add(offset).cast::<T>();
            &*elt
        }
    }
}

impl<T> IndexMut<usize> for [T] {
    fn index_mut(&mut self, idx: usize) -> &mut T {
        if idx >= ¤slice_len::<T>(self) {
            panic!("slice index out of bounds")
        }
        let offset: usize = idx * mem::size_of::<T>();
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            let elt: *mut T = base.cast::<u8>().byte_add(offset).cast::<T>();
            &mut *elt
        }
    }
}

// `arr[start..end]` etc. — Range slicing for `[T]`. Each impl
// bounds-checks then constructs a sub-slice via the `¤make_slice` /
// `¤make_mut_slice` raw-parts intrinsics. Out-of-range or reversed
// indices `panic!`. The `Output` is the slice type itself (`[T]`),
// so the caller gets `&[T]` / `&mut [T]` after the autoderef the
// indexing codegen does. Repetitive across the six range shapes —
// the variation is just which bounds to check and which start/end
// to use; structure is the same.

impl<T> Index<Range<usize>> for [T] {
    type Output = [T];
    fn index(&self, r: Range<usize>) -> &[T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.start > r.end {
            panic!("slice range start > end")
        }
        if r.end > len {
            panic!("slice range end out of bounds")
        }
        let new_len: usize = r.end - r.start;
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            let new_ptr: *const u8 = base.cast::<u8>().byte_add(r.start * mem::size_of::<T>());
            ¤make_slice::<T>(new_ptr, new_len)
        }
    }
}

impl<T> IndexMut<Range<usize>> for [T] {
    fn index_mut(&mut self, r: Range<usize>) -> &mut [T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.start > r.end {
            panic!("slice range start > end")
        }
        if r.end > len {
            panic!("slice range end out of bounds")
        }
        let new_len: usize = r.end - r.start;
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            let new_ptr: *mut u8 = base.cast::<u8>().byte_add(r.start * mem::size_of::<T>());
            ¤make_mut_slice::<T>(new_ptr, new_len)
        }
    }
}

impl<T> Index<RangeFrom<usize>> for [T] {
    type Output = [T];
    fn index(&self, r: RangeFrom<usize>) -> &[T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.start > len {
            panic!("slice range start out of bounds")
        }
        let new_len: usize = len - r.start;
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            let new_ptr: *const u8 = base.cast::<u8>().byte_add(r.start * mem::size_of::<T>());
            ¤make_slice::<T>(new_ptr, new_len)
        }
    }
}

impl<T> IndexMut<RangeFrom<usize>> for [T] {
    fn index_mut(&mut self, r: RangeFrom<usize>) -> &mut [T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.start > len {
            panic!("slice range start out of bounds")
        }
        let new_len: usize = len - r.start;
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            let new_ptr: *mut u8 = base.cast::<u8>().byte_add(r.start * mem::size_of::<T>());
            ¤make_mut_slice::<T>(new_ptr, new_len)
        }
    }
}

impl<T> Index<RangeTo<usize>> for [T] {
    type Output = [T];
    fn index(&self, r: RangeTo<usize>) -> &[T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.end > len {
            panic!("slice range end out of bounds")
        }
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            ¤make_slice::<T>(base.cast::<u8>(), r.end)
        }
    }
}

impl<T> IndexMut<RangeTo<usize>> for [T] {
    fn index_mut(&mut self, r: RangeTo<usize>) -> &mut [T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.end > len {
            panic!("slice range end out of bounds")
        }
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            ¤make_mut_slice::<T>(base.cast::<u8>(), r.end)
        }
    }
}

impl<T> Index<RangeInclusive<usize>> for [T] {
    type Output = [T];
    fn index(&self, r: RangeInclusive<usize>) -> &[T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.start > r.end {
            panic!("slice range start > end")
        }
        if r.end >= len {
            panic!("slice range end out of bounds")
        }
        let new_len: usize = r.end - r.start + 1;
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            let new_ptr: *const u8 = base.cast::<u8>().byte_add(r.start * mem::size_of::<T>());
            ¤make_slice::<T>(new_ptr, new_len)
        }
    }
}

impl<T> IndexMut<RangeInclusive<usize>> for [T] {
    fn index_mut(&mut self, r: RangeInclusive<usize>) -> &mut [T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.start > r.end {
            panic!("slice range start > end")
        }
        if r.end >= len {
            panic!("slice range end out of bounds")
        }
        let new_len: usize = r.end - r.start + 1;
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            let new_ptr: *mut u8 = base.cast::<u8>().byte_add(r.start * mem::size_of::<T>());
            ¤make_mut_slice::<T>(new_ptr, new_len)
        }
    }
}

impl<T> Index<RangeToInclusive<usize>> for [T] {
    type Output = [T];
    fn index(&self, r: RangeToInclusive<usize>) -> &[T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.end >= len {
            panic!("slice range end out of bounds")
        }
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            ¤make_slice::<T>(base.cast::<u8>(), r.end + 1)
        }
    }
}

impl<T> IndexMut<RangeToInclusive<usize>> for [T] {
    fn index_mut(&mut self, r: RangeToInclusive<usize>) -> &mut [T] {
        let len: usize = ¤slice_len::<T>(self);
        if r.end >= len {
            panic!("slice range end out of bounds")
        }
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            ¤make_mut_slice::<T>(base.cast::<u8>(), r.end + 1)
        }
    }
}

impl<T> Index<RangeFull> for [T] {
    type Output = [T];
    fn index(&self, _r: RangeFull) -> &[T] {
        let len: usize = ¤slice_len::<T>(self);
        unsafe {
            let base: *const T = ¤slice_ptr::<T>(self);
            ¤make_slice::<T>(base.cast::<u8>(), len)
        }
    }
}

impl<T> IndexMut<RangeFull> for [T] {
    fn index_mut(&mut self, _r: RangeFull) -> &mut [T] {
        let len: usize = ¤slice_len::<T>(self);
        unsafe {
            let base: *mut T = ¤slice_mut_ptr::<T>(self);
            ¤make_mut_slice::<T>(base.cast::<u8>(), len)
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
