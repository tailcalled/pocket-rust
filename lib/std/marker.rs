pub trait Copy {}

impl Copy for u8 {}
impl Copy for i8 {}
impl Copy for u16 {}
impl Copy for i16 {}
impl Copy for u32 {}
impl Copy for i32 {}
impl Copy for u64 {}
impl Copy for i64 {}
impl Copy for u128 {}
impl Copy for i128 {}
impl Copy for usize {}
impl Copy for isize {}

impl<T> Copy for &T {}

impl<T> Copy for *const T {}
impl<T> Copy for *mut T {}
