use crate::mem;
use crate::ops::Deref;
use crate::ops::DerefMut;
use crate::ops::Drop;

// Heap-allocated single-value smart pointer. Owns its T uniquely:
// `Box::new(value)` allocates `size_of::<T>()` bytes via `¤alloc`,
// moves `value` onto the heap, and returns a `Box<T>` whose `Drop`
// runs T's destructor (if any) and frees the buffer.
//
// `*box` reads/writes the inner T via the `Deref` / `DerefMut` impls
// below — both bodies turn the raw `*mut T` back into a `&T` /
// `&mut T` via an `unsafe { &*p }` / `unsafe { &mut *p }`. Borrowck
// trusts the Box wrapper to enforce uniqueness; the compile-time
// invariant is that you can't mint two `&mut *box` simultaneously
// because `deref_mut` takes `&mut self`.
//
// Drop-bypass mechanism: `into_raw` / `into_inner` / `leak` need to
// hand off ownership of the buffer (or the inner T) without the
// Box's Drop firing afterwards. We achieve this by re-binding the
// incoming `Box<T>` as a `let mut local`, mutating `local.ptr` to
// null, and relying on the Drop impl's null check below to skip the
// free / inner-drop sequence.

pub struct Box<T> {
    ptr: *mut T,
}

impl<T> Box<T> {
    // Allocate space for one T on the heap and move `value` into it.
    pub fn new(value: T) -> Box<T> {
        let size: usize = mem::size_of::<T>();
        let bytes: *mut u8 = unsafe { ¤alloc(size) };
        let ptr: *mut T = unsafe { bytes.cast::<T>() };
        unsafe { *ptr = value; }
        Box { ptr }
    }

    // Consume the Box and return its raw `*mut T`. The caller takes
    // ownership of the heap allocation; Drop won't run for the
    // inner T or free the buffer. Wrap the returned ptr back in a
    // Box via `Box::from_raw` to re-establish ownership.
    pub fn into_raw(b: Box<T>) -> *mut T {
        let mut local: Box<T> = b;
        let p: *mut T = local.ptr;
        local.ptr = 0 as *mut T;
        p
    }

    // Wrap a raw `*mut T` back into a `Box<T>`. Caller asserts that
    // `raw` was obtained from `Box::into_raw` (or directly from
    // `¤alloc(size_of::<T>())` with a properly initialized T at
    // that address). Unsafe because the wrong size / uninitialized
    // T / double-free would all corrupt the heap.
    pub unsafe fn from_raw(raw: *mut T) -> Box<T> {
        Box { ptr: raw }
    }

    // Consume the Box, return the inner T, free the buffer. T's
    // destructor doesn't run (the value is moved out to the caller).
    // The deref-read goes through a separate `*mut T` binding, not
    // through `local.ptr` directly — borrowck's
    // partial-move-of-Drop check fires when the move's place is
    // rooted at a Drop type, which `local: Box<T>` is. Routing
    // through a non-Drop `*mut T` local sidesteps that.
    pub fn into_inner(b: Box<T>) -> T {
        let mut local: Box<T> = b;
        let raw: *mut T = local.ptr;
        local.ptr = 0 as *mut T;
        let v: T = unsafe { *raw };
        unsafe { ¤free(raw.cast::<u8>()); }
        v
    }

    // Borrow the inner allocation as a `*const T`. Doesn't consume
    // the Box; the returned ptr is valid as long as the Box is.
    pub fn as_ptr(b: &Box<T>) -> *const T {
        b.ptr as *const T
    }

    // Borrow the inner allocation as a `*mut T`. Same liveness rule.
    pub fn as_mut_ptr(b: &mut Box<T>) -> *mut T {
        b.ptr
    }

    // Consume the Box and return a `&'static mut T` to the inner
    // allocation. The buffer is *leaked* — never freed and T's
    // destructor never runs. Conceptually safe because pocket-rust
    // doesn't enforce real lifetime correctness today, and the
    // returned ref's "static" lifetime reflects that the storage
    // lives forever.
    pub fn leak(b: Box<T>) -> &'static mut T {
        let p: *mut T = Box::into_raw(b);
        unsafe { &mut *p }
    }

    // TODO: new_uninit(self) -> Box<MaybeUninit<T>> — needs `MaybeUninit`.
    // TODO: new_zeroed() -> Box<MaybeUninit<T>> — needs `MaybeUninit` plus a zeroed-alloc intrinsic.
    // TODO: new_uninit_slice(len) -> Box<[MaybeUninit<T>]> — needs `MaybeUninit` and slice-Box ABI.
    // TODO: new_zeroed_slice(len) -> Box<[MaybeUninit<T>]> — same as above.
    // TODO: pin(value) -> Pin<Box<T>> — needs `Pin`.
    // TODO: try_new(value) -> Result<Self, AllocError> — needs allocation-error type / fallible alloc intrinsic; pocket-rust's `¤alloc` panics-or-traps on OOM rather than returning a status.
    // TODO: try_new_uninit / try_new_zeroed / try_pin — same reasons.
    // TODO: assume_init(self) on Box<MaybeUninit<T>> — needs `MaybeUninit`.
    // TODO: write(boxed, value) on Box<MaybeUninit<T>> — needs `MaybeUninit`.
    // TODO: downcast(self) on `Box<dyn Any>` — needs `Any` and `dyn Trait`.
    // TODO: from_raw_parts(ptr, alloc) — needs custom allocator support.
    // TODO: into_raw_with_allocator / from_raw_in / new_in / try_new_in / pin_in — same.
    // TODO: into_pin(self) -> Pin<Box<T>> — needs `Pin`.
    // TODO: from(value) (the `From<T> for Box<T>` impl) — needs `From` trait.
    // TODO: clone(&self) -> Box<T> where T: Clone — needs `Clone`.
}

impl<T> Deref for Box<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.ptr }
    }
}

impl<T> DerefMut for Box<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.ptr }
    }
}

impl<T> Drop for Box<T> {
    fn drop(&mut self) {
        // Null-ptr sentinel: `into_raw` / `into_inner` / `leak` zero
        // out `self.ptr` to signal that ownership has been handed
        // off, so Drop becomes a no-op.
        if self.ptr.is_null() { return; }
        // Save the buffer pointer first (raw pointers are Copy, so
        // the `Box::ptr` field stays readable for the free below
        // even after the deref-move on the next line).
        let buf: *mut u8 = self.ptr.cast::<u8>();
        // Move T off the heap into a local so the existing scope-end
        // Drop machinery runs T's destructor (a no-op for non-Drop
        // types; the call to `mem::drop` makes that explicit).
        let v: T = unsafe { *self.ptr };
        mem::drop(v);
        unsafe { ¤free(buf); }
    }
}

// TODO: impl<T: Clone> Clone for Box<T> — needs `Clone`.
// TODO: impl<T: PartialEq> PartialEq for Box<T> — needs the chained PartialEq dispatch through Deref to work uniformly.
// TODO: impl<T: Eq> Eq for Box<T> — same.
// TODO: impl<T: PartialOrd> PartialOrd for Box<T> — same plus the lex-order story.
// TODO: impl<T: Ord> Ord for Box<T> — same.
// TODO: impl<T: Hash> Hash for Box<T> — needs `Hash`.
// TODO: impl<T: Debug> Debug for Box<T> — needs `Debug`.
// TODO: impl<T: Display> Display for Box<T> — needs `Display`.
// TODO: impl<T: Default> Default for Box<T> — needs `Default`.
// TODO: impl<T> From<T> for Box<T> — needs `From`.
// TODO: impl<I: Iterator> Iterator for Box<I> — needs `Iterator`.
// TODO: impl<F: Future> Future for Box<F> — needs `Future` / async machinery.
