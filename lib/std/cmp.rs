pub trait PartialEq {
    fn eq(&self, other: &Self) -> bool;
    fn ne(&self, other: &Self) -> bool;
}

pub trait Eq: PartialEq {}

pub trait PartialOrd: PartialEq {
    fn lt(&self, other: &Self) -> bool;
    fn le(&self, other: &Self) -> bool;
    fn gt(&self, other: &Self) -> bool;
    fn ge(&self, other: &Self) -> bool;
}

pub trait Ord: PartialOrd + Eq {}

impl PartialEq for u8 {
    fn eq(&self, other: &u8) -> bool { ¤u8_eq(*self, *other) }
    fn ne(&self, other: &u8) -> bool { ¤u8_ne(*self, *other) }
}
impl Eq for u8 {}
impl PartialOrd for u8 {
    fn lt(&self, other: &u8) -> bool { ¤u8_lt(*self, *other) }
    fn le(&self, other: &u8) -> bool { ¤u8_le(*self, *other) }
    fn gt(&self, other: &u8) -> bool { ¤u8_gt(*self, *other) }
    fn ge(&self, other: &u8) -> bool { ¤u8_ge(*self, *other) }
}
impl Ord for u8 {}

impl PartialEq for i8 {
    fn eq(&self, other: &i8) -> bool { ¤i8_eq(*self, *other) }
    fn ne(&self, other: &i8) -> bool { ¤i8_ne(*self, *other) }
}
impl Eq for i8 {}
impl PartialOrd for i8 {
    fn lt(&self, other: &i8) -> bool { ¤i8_lt(*self, *other) }
    fn le(&self, other: &i8) -> bool { ¤i8_le(*self, *other) }
    fn gt(&self, other: &i8) -> bool { ¤i8_gt(*self, *other) }
    fn ge(&self, other: &i8) -> bool { ¤i8_ge(*self, *other) }
}
impl Ord for i8 {}

impl PartialEq for u16 {
    fn eq(&self, other: &u16) -> bool { ¤u16_eq(*self, *other) }
    fn ne(&self, other: &u16) -> bool { ¤u16_ne(*self, *other) }
}
impl Eq for u16 {}
impl PartialOrd for u16 {
    fn lt(&self, other: &u16) -> bool { ¤u16_lt(*self, *other) }
    fn le(&self, other: &u16) -> bool { ¤u16_le(*self, *other) }
    fn gt(&self, other: &u16) -> bool { ¤u16_gt(*self, *other) }
    fn ge(&self, other: &u16) -> bool { ¤u16_ge(*self, *other) }
}
impl Ord for u16 {}

impl PartialEq for i16 {
    fn eq(&self, other: &i16) -> bool { ¤i16_eq(*self, *other) }
    fn ne(&self, other: &i16) -> bool { ¤i16_ne(*self, *other) }
}
impl Eq for i16 {}
impl PartialOrd for i16 {
    fn lt(&self, other: &i16) -> bool { ¤i16_lt(*self, *other) }
    fn le(&self, other: &i16) -> bool { ¤i16_le(*self, *other) }
    fn gt(&self, other: &i16) -> bool { ¤i16_gt(*self, *other) }
    fn ge(&self, other: &i16) -> bool { ¤i16_ge(*self, *other) }
}
impl Ord for i16 {}

impl PartialEq for u32 {
    fn eq(&self, other: &u32) -> bool { ¤u32_eq(*self, *other) }
    fn ne(&self, other: &u32) -> bool { ¤u32_ne(*self, *other) }
}
impl Eq for u32 {}
impl PartialOrd for u32 {
    fn lt(&self, other: &u32) -> bool { ¤u32_lt(*self, *other) }
    fn le(&self, other: &u32) -> bool { ¤u32_le(*self, *other) }
    fn gt(&self, other: &u32) -> bool { ¤u32_gt(*self, *other) }
    fn ge(&self, other: &u32) -> bool { ¤u32_ge(*self, *other) }
}
impl Ord for u32 {}

impl PartialEq for i32 {
    fn eq(&self, other: &i32) -> bool { ¤i32_eq(*self, *other) }
    fn ne(&self, other: &i32) -> bool { ¤i32_ne(*self, *other) }
}
impl Eq for i32 {}
impl PartialOrd for i32 {
    fn lt(&self, other: &i32) -> bool { ¤i32_lt(*self, *other) }
    fn le(&self, other: &i32) -> bool { ¤i32_le(*self, *other) }
    fn gt(&self, other: &i32) -> bool { ¤i32_gt(*self, *other) }
    fn ge(&self, other: &i32) -> bool { ¤i32_ge(*self, *other) }
}
impl Ord for i32 {}

impl PartialEq for u64 {
    fn eq(&self, other: &u64) -> bool { ¤u64_eq(*self, *other) }
    fn ne(&self, other: &u64) -> bool { ¤u64_ne(*self, *other) }
}
impl Eq for u64 {}
impl PartialOrd for u64 {
    fn lt(&self, other: &u64) -> bool { ¤u64_lt(*self, *other) }
    fn le(&self, other: &u64) -> bool { ¤u64_le(*self, *other) }
    fn gt(&self, other: &u64) -> bool { ¤u64_gt(*self, *other) }
    fn ge(&self, other: &u64) -> bool { ¤u64_ge(*self, *other) }
}
impl Ord for u64 {}

impl PartialEq for i64 {
    fn eq(&self, other: &i64) -> bool { ¤i64_eq(*self, *other) }
    fn ne(&self, other: &i64) -> bool { ¤i64_ne(*self, *other) }
}
impl Eq for i64 {}
impl PartialOrd for i64 {
    fn lt(&self, other: &i64) -> bool { ¤i64_lt(*self, *other) }
    fn le(&self, other: &i64) -> bool { ¤i64_le(*self, *other) }
    fn gt(&self, other: &i64) -> bool { ¤i64_gt(*self, *other) }
    fn ge(&self, other: &i64) -> bool { ¤i64_ge(*self, *other) }
}
impl Ord for i64 {}

impl PartialEq for u128 {
    fn eq(&self, other: &u128) -> bool { ¤u128_eq(*self, *other) }
    fn ne(&self, other: &u128) -> bool { ¤u128_ne(*self, *other) }
}
impl Eq for u128 {}
impl PartialOrd for u128 {
    fn lt(&self, other: &u128) -> bool { ¤u128_lt(*self, *other) }
    fn le(&self, other: &u128) -> bool { ¤u128_le(*self, *other) }
    fn gt(&self, other: &u128) -> bool { ¤u128_gt(*self, *other) }
    fn ge(&self, other: &u128) -> bool { ¤u128_ge(*self, *other) }
}
impl Ord for u128 {}

impl PartialEq for i128 {
    fn eq(&self, other: &i128) -> bool { ¤i128_eq(*self, *other) }
    fn ne(&self, other: &i128) -> bool { ¤i128_ne(*self, *other) }
}
impl Eq for i128 {}
impl PartialOrd for i128 {
    fn lt(&self, other: &i128) -> bool { ¤i128_lt(*self, *other) }
    fn le(&self, other: &i128) -> bool { ¤i128_le(*self, *other) }
    fn gt(&self, other: &i128) -> bool { ¤i128_gt(*self, *other) }
    fn ge(&self, other: &i128) -> bool { ¤i128_ge(*self, *other) }
}
impl Ord for i128 {}

impl PartialEq for usize {
    fn eq(&self, other: &usize) -> bool { ¤usize_eq(*self, *other) }
    fn ne(&self, other: &usize) -> bool { ¤usize_ne(*self, *other) }
}
impl Eq for usize {}
impl PartialOrd for usize {
    fn lt(&self, other: &usize) -> bool { ¤usize_lt(*self, *other) }
    fn le(&self, other: &usize) -> bool { ¤usize_le(*self, *other) }
    fn gt(&self, other: &usize) -> bool { ¤usize_gt(*self, *other) }
    fn ge(&self, other: &usize) -> bool { ¤usize_ge(*self, *other) }
}
impl Ord for usize {}

impl PartialEq for isize {
    fn eq(&self, other: &isize) -> bool { ¤isize_eq(*self, *other) }
    fn ne(&self, other: &isize) -> bool { ¤isize_ne(*self, *other) }
}
impl Eq for isize {}
impl PartialOrd for isize {
    fn lt(&self, other: &isize) -> bool { ¤isize_lt(*self, *other) }
    fn le(&self, other: &isize) -> bool { ¤isize_le(*self, *other) }
    fn gt(&self, other: &isize) -> bool { ¤isize_gt(*self, *other) }
    fn ge(&self, other: &isize) -> bool { ¤isize_ge(*self, *other) }
}
impl Ord for isize {}

impl PartialEq for bool {
    fn eq(&self, other: &bool) -> bool { ¤bool_eq(*self, *other) }
    fn ne(&self, other: &bool) -> bool { ¤bool_ne(*self, *other) }
}
impl Eq for bool {}

// `char` is a Unicode scalar value (0..=0x10FFFF excluding surrogates),
// stored as a u32 — so the comparison ops route through the u32
// builtins after an `as u32` cast on each side.
impl PartialEq for char {
    fn eq(&self, other: &char) -> bool { ¤u32_eq(*self as u32, *other as u32) }
    fn ne(&self, other: &char) -> bool { ¤u32_ne(*self as u32, *other as u32) }
}
impl Eq for char {}
impl PartialOrd for char {
    fn lt(&self, other: &char) -> bool { ¤u32_lt(*self as u32, *other as u32) }
    fn le(&self, other: &char) -> bool { ¤u32_le(*self as u32, *other as u32) }
    fn gt(&self, other: &char) -> bool { ¤u32_gt(*self as u32, *other as u32) }
    fn ge(&self, other: &char) -> bool { ¤u32_ge(*self as u32, *other as u32) }
}
impl Ord for char {}

// `&str` equality. Implemented on `&str` directly so dispatch lands
// at recv-autoref level 1 (where both recv and arg become `&&str`)
// instead of the `impl PartialEq for str` shape, which would mismatch
// on the arg side because the comparison desugar wraps rhs in `&`.
// Walks the str fat-ref's bytes after a length-equality short-circuit.
impl PartialEq for &str {
    fn eq(&self, other: &&str) -> bool {
        let a: &str = *self;
        let b: &str = *other;
        let n: usize = ¤str_len(a);
        let m: usize = ¤str_len(b);
        if ¤usize_ne(n, m) { return false; }
        let pa: *const u8 = ¤slice_ptr::<u8>(¤str_as_bytes(a));
        let pb: *const u8 = ¤slice_ptr::<u8>(¤str_as_bytes(b));
        let mut i: usize = 0;
        while ¤usize_lt(i, n) {
            let ba: u8 = unsafe { *(pa.byte_add(i)) };
            let bb: u8 = unsafe { *(pb.byte_add(i)) };
            if ¤u8_ne(ba, bb) { return false; }
            i = ¤usize_add(i, 1);
        }
        true
    }
    fn ne(&self, other: &&str) -> bool { ¤bool_not(self.eq(other)) }
}
impl Eq for &str {}

// TODO: impl PartialOrd / Ord for &str — lexicographic byte compare;
// needs a "first differing byte" walk before the length tiebreak.
