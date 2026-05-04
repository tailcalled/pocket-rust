// Clone is the explicit-duplication trait. Distinct from `Copy` (which
// is implicit bitwise duplication): a Clone impl can do work — heap
// reallocation, refcount bumps, etc. The `#[derive(Clone)]` attribute
// generates an impl that recursively clones each field.

pub trait Clone {
    fn clone(&self) -> Self;
}

impl Clone for u8 { fn clone(&self) -> u8 { *self } }
impl Clone for i8 { fn clone(&self) -> i8 { *self } }
impl Clone for u16 { fn clone(&self) -> u16 { *self } }
impl Clone for i16 { fn clone(&self) -> i16 { *self } }
impl Clone for u32 { fn clone(&self) -> u32 { *self } }
impl Clone for i32 { fn clone(&self) -> i32 { *self } }
impl Clone for u64 { fn clone(&self) -> u64 { *self } }
impl Clone for i64 { fn clone(&self) -> i64 { *self } }
impl Clone for u128 { fn clone(&self) -> u128 { *self } }
impl Clone for i128 { fn clone(&self) -> i128 { *self } }
impl Clone for usize { fn clone(&self) -> usize { *self } }
impl Clone for isize { fn clone(&self) -> isize { *self } }
impl Clone for bool { fn clone(&self) -> bool { *self } }
impl Clone for char { fn clone(&self) -> char { *self } }

// Shared refs are always Copy and trivially Clone — `*self` is `&T`
// (a reborrow of the inner reference, which is Copy). Mut refs are
// not Clone (they're exclusive); pocket-rust matches Rust here.
impl<T> Clone for &T { fn clone(&self) -> &T { *self } }
impl<T> Clone for *const T { fn clone(&self) -> *const T { *self } }
impl<T> Clone for *mut T { fn clone(&self) -> *mut T { *self } }
