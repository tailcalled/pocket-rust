// `Vec<T>` — a heap-allocated, dynamically resizable array.
//
// Layout: a raw pointer to the buffer, the number of initialized
// elements (`len`), and the allocated capacity (`cap`). Elements are
// stored densely starting at `ptr`; element `i` lives at the byte
// address `ptr + i * mem::size_of::<T>()`.
//
// Memory management: `¤alloc` (bump-allocator) backs the buffer;
// `¤free` is currently a no-op, so a Vec's storage isn't reclaimed
// when it grows or drops. Element destructors *are* called via
// `mem::drop` — that's separate from buffer reclamation. (Once the
// stdlib gains an `alloc` module wrapping `¤alloc` / `¤free`, this
// file should switch to it.)
//
// All pointer arithmetic and raw-pointer reads/writes happen inside
// `unsafe` blocks. The public API exposes only safe operations.

use crate::mem;
use crate::option::Option;
use crate::ops::Drop;

pub struct Vec<T> {
    ptr: *mut T,
    len: usize,
    cap: usize,
}

impl<T> Vec<T> {
    pub fn new() -> Vec<T> {
        // `0 as *mut u8` produces the null pointer; `.cast::<T>()`
        // retypes it to `*mut T` without changing the wasm value.
        let null_u8: *mut u8 = 0 as *mut u8;
        Vec {
            ptr: null_u8.cast::<T>(),
            len: 0,
            cap: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    // Borrow the initialized prefix as a `&[T]` slice. The returned
    // fat ref carries (data ptr, length); both halves are read out of
    // `self`. Lifetime-tied to `&self`.
    pub fn as_slice(&self) -> &[T] {
        unsafe { ¤make_slice::<T>(self.ptr.cast::<u8>() as *const u8, self.len) }
    }

    // Mutable counterpart of `as_slice`. Returns `&mut [T]` whose
    // exclusive borrow is tied to `&mut self`.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { ¤make_mut_slice::<T>(self.ptr.cast::<u8>(), self.len) }
    }

    pub fn push(&mut self, value: T) {
        if self.len == self.cap {
            self.grow();
        }
        let offset: usize = self.len * mem::size_of::<T>();
        unsafe {
            let buf_u8: *mut u8 = self.ptr.cast::<u8>();
            let dst: *mut T = buf_u8.byte_add(offset).cast::<T>();
            *dst = value;
        }
        self.len = self.len + 1;
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            Option::None
        } else {
            self.len = self.len - 1;
            let offset: usize = self.len * mem::size_of::<T>();
            unsafe {
                let buf_u8: *mut u8 = self.ptr.cast::<u8>();
                let src: *mut T = buf_u8.byte_add(offset).cast::<T>();
                Option::Some(*src)
            }
        }
    }

    pub fn get(&self, idx: usize) -> Option<&T> {
        if idx >= self.len {
            Option::None
        } else {
            let offset: usize = idx * mem::size_of::<T>();
            unsafe {
                let buf_u8: *mut u8 = self.ptr.cast::<u8>();
                let elt: *const T = buf_u8.byte_add(offset).cast::<T>() as *const T;
                Option::Some(&*elt)
            }
        }
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        if idx >= self.len {
            Option::None
        } else {
            let offset: usize = idx * mem::size_of::<T>();
            unsafe {
                let buf_u8: *mut u8 = self.ptr.cast::<u8>();
                let elt: *mut T = buf_u8.byte_add(offset).cast::<T>();
                Option::Some(&mut *elt)
            }
        }
    }

    pub fn clear(&mut self) {
        let elem_size: usize = mem::size_of::<T>();
        let mut i: usize = 0;
        while i < self.len {
            let offset: usize = i * elem_size;
            unsafe {
                let buf_u8: *mut u8 = self.ptr.cast::<u8>();
                let src: *mut T = buf_u8.byte_add(offset).cast::<T>();
                mem::drop::<T>(*src);
            }
            i = i + 1;
        }
        self.len = 0;
    }

    fn grow(&mut self) {
        let new_cap: usize = if self.cap == 0 {
            4
        } else {
            self.cap * 2
        };
        let elem_size: usize = mem::size_of::<T>();
        let new_buf_u8: *mut u8 = unsafe { ¤alloc(new_cap * elem_size) };
        // Bytewise copy of the existing initialized prefix into the
        // new buffer. Each element is read via raw deref (no Drop fires
        // — borrowck doesn't track raw-pointer moves) and stored at the
        // matching offset in the new buffer. After the loop the old
        // bytes are abandoned and `¤free`d (which is a no-op today).
        let mut i: usize = 0;
        while i < self.len {
            let offset: usize = i * elem_size;
            unsafe {
                let old_buf_u8: *mut u8 = self.ptr.cast::<u8>();
                let src: *mut T = old_buf_u8.byte_add(offset).cast::<T>();
                let dst: *mut T = new_buf_u8.byte_add(offset).cast::<T>();
                *dst = *src;
            }
            i = i + 1;
        }
        unsafe {
            ¤free(self.ptr.cast::<u8>());
        }
        self.ptr = new_buf_u8.cast::<T>();
        self.cap = new_cap;
    }
}

impl<T> Drop for Vec<T> {
    fn drop(&mut self) {
        // Drop each initialized element by reading it through the raw
        // pointer and consuming it via `mem::drop`. For non-Drop T this
        // is a series of no-op loads; for Drop T each load triggers
        // T's destructor inside `mem::drop`'s scope-end machinery.
        let elem_size: usize = mem::size_of::<T>();
        let mut i: usize = 0;
        while i < self.len {
            let offset: usize = i * elem_size;
            unsafe {
                let buf_u8: *mut u8 = self.ptr.cast::<u8>();
                let src: *mut T = buf_u8.byte_add(offset).cast::<T>();
                mem::drop::<T>(*src);
            }
            i = i + 1;
        }
        unsafe {
            ¤free(self.ptr.cast::<u8>());
        }
    }
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically. When
// a blocker lands, search this file for the relevant TODO.
//
// TODO: append(&mut self, other: &mut Vec<T>) — needs draining; expressible once the move-out-of-`*mut` story for `take`/`replace` is sound.
// TODO: as_mut_slice(&mut self) -> &mut [T] — same shape as `as_slice`, but needs the codegen path for `&mut [T]` (mutable fat ref) to be wired through; the immutable case landed first.
// TODO: contains(&self, x: &T) — needs the `PartialEq` constraint on element comparison; expressible today, just hadn't a use case.
// TODO: dedup / dedup_by / dedup_by_key — needs PartialEq + closures.
// TODO: drain(&mut self, range) — needs ranges as first-class values + iterator support.
// TODO: extend(&mut self, iter) / extend_from_slice — needs iterator traits + slices.
// TODO: from_iter(iter) — needs iterator traits.
// TODO: insert(&mut self, idx, value) / remove(&mut self, idx) — needs in-place memmove of the tail; expressible via raw arithmetic but not bootstrap-critical.
// TODO: into_iter(self) / iter(&self) / iter_mut(&mut self) — needs iterator traits.
// TODO: last(&self) -> Option<&T> / first(&self) — trivial wrappers around get; add when there's a caller.
// TODO: leak(self) -> &'static mut [T] — needs slice + 'static lifetime + a way to suppress Drop on `self`.
// TODO: reserve(&mut self, additional) / reserve_exact / shrink_to / shrink_to_fit / try_reserve* — needs callers; today we always grow on demand.
// TODO: resize(&mut self, new_len, value) — needs `Clone` on `T`.
// TODO: retain(&mut self, f) / retain_mut — needs closures.
// TODO: set_len(&mut self, new_len) — straightforward but unsafe; add when needed.
// TODO: split_off(&mut self, at) — needs allocation of a fresh Vec by sharing the buffer prefix; achievable but not bootstrap-critical.
// TODO: swap_remove(&mut self, idx) — expressible today (read tail into idx, decrement len); add when needed.
// TODO: truncate(&mut self, new_len) — expressible today (drop elements idx..len, set len); add when needed.
// TODO: with_capacity(cap) — needs to call `¤alloc` eagerly; trivial but `new()` + repeated `push` is fine for bootstrap.
