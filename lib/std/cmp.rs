pub trait Eq {
    fn eq(&self, other: &Self) -> bool;
    fn ne(&self, other: &Self) -> bool;
}

pub trait Ord {
    fn lt(&self, other: &Self) -> bool;
    fn le(&self, other: &Self) -> bool;
    fn gt(&self, other: &Self) -> bool;
    fn ge(&self, other: &Self) -> bool;
}

impl Eq for u8 {
    fn eq(&self, other: &u8) -> bool { ¤u8_eq(*self, *other) }
    fn ne(&self, other: &u8) -> bool { ¤u8_ne(*self, *other) }
}
impl Ord for u8 {
    fn lt(&self, other: &u8) -> bool { ¤u8_lt(*self, *other) }
    fn le(&self, other: &u8) -> bool { ¤u8_le(*self, *other) }
    fn gt(&self, other: &u8) -> bool { ¤u8_gt(*self, *other) }
    fn ge(&self, other: &u8) -> bool { ¤u8_ge(*self, *other) }
}

impl Eq for i8 {
    fn eq(&self, other: &i8) -> bool { ¤i8_eq(*self, *other) }
    fn ne(&self, other: &i8) -> bool { ¤i8_ne(*self, *other) }
}
impl Ord for i8 {
    fn lt(&self, other: &i8) -> bool { ¤i8_lt(*self, *other) }
    fn le(&self, other: &i8) -> bool { ¤i8_le(*self, *other) }
    fn gt(&self, other: &i8) -> bool { ¤i8_gt(*self, *other) }
    fn ge(&self, other: &i8) -> bool { ¤i8_ge(*self, *other) }
}

impl Eq for u16 {
    fn eq(&self, other: &u16) -> bool { ¤u16_eq(*self, *other) }
    fn ne(&self, other: &u16) -> bool { ¤u16_ne(*self, *other) }
}
impl Ord for u16 {
    fn lt(&self, other: &u16) -> bool { ¤u16_lt(*self, *other) }
    fn le(&self, other: &u16) -> bool { ¤u16_le(*self, *other) }
    fn gt(&self, other: &u16) -> bool { ¤u16_gt(*self, *other) }
    fn ge(&self, other: &u16) -> bool { ¤u16_ge(*self, *other) }
}

impl Eq for i16 {
    fn eq(&self, other: &i16) -> bool { ¤i16_eq(*self, *other) }
    fn ne(&self, other: &i16) -> bool { ¤i16_ne(*self, *other) }
}
impl Ord for i16 {
    fn lt(&self, other: &i16) -> bool { ¤i16_lt(*self, *other) }
    fn le(&self, other: &i16) -> bool { ¤i16_le(*self, *other) }
    fn gt(&self, other: &i16) -> bool { ¤i16_gt(*self, *other) }
    fn ge(&self, other: &i16) -> bool { ¤i16_ge(*self, *other) }
}

impl Eq for u32 {
    fn eq(&self, other: &u32) -> bool { ¤u32_eq(*self, *other) }
    fn ne(&self, other: &u32) -> bool { ¤u32_ne(*self, *other) }
}
impl Ord for u32 {
    fn lt(&self, other: &u32) -> bool { ¤u32_lt(*self, *other) }
    fn le(&self, other: &u32) -> bool { ¤u32_le(*self, *other) }
    fn gt(&self, other: &u32) -> bool { ¤u32_gt(*self, *other) }
    fn ge(&self, other: &u32) -> bool { ¤u32_ge(*self, *other) }
}

impl Eq for i32 {
    fn eq(&self, other: &i32) -> bool { ¤i32_eq(*self, *other) }
    fn ne(&self, other: &i32) -> bool { ¤i32_ne(*self, *other) }
}
impl Ord for i32 {
    fn lt(&self, other: &i32) -> bool { ¤i32_lt(*self, *other) }
    fn le(&self, other: &i32) -> bool { ¤i32_le(*self, *other) }
    fn gt(&self, other: &i32) -> bool { ¤i32_gt(*self, *other) }
    fn ge(&self, other: &i32) -> bool { ¤i32_ge(*self, *other) }
}

impl Eq for u64 {
    fn eq(&self, other: &u64) -> bool { ¤u64_eq(*self, *other) }
    fn ne(&self, other: &u64) -> bool { ¤u64_ne(*self, *other) }
}
impl Ord for u64 {
    fn lt(&self, other: &u64) -> bool { ¤u64_lt(*self, *other) }
    fn le(&self, other: &u64) -> bool { ¤u64_le(*self, *other) }
    fn gt(&self, other: &u64) -> bool { ¤u64_gt(*self, *other) }
    fn ge(&self, other: &u64) -> bool { ¤u64_ge(*self, *other) }
}

impl Eq for i64 {
    fn eq(&self, other: &i64) -> bool { ¤i64_eq(*self, *other) }
    fn ne(&self, other: &i64) -> bool { ¤i64_ne(*self, *other) }
}
impl Ord for i64 {
    fn lt(&self, other: &i64) -> bool { ¤i64_lt(*self, *other) }
    fn le(&self, other: &i64) -> bool { ¤i64_le(*self, *other) }
    fn gt(&self, other: &i64) -> bool { ¤i64_gt(*self, *other) }
    fn ge(&self, other: &i64) -> bool { ¤i64_ge(*self, *other) }
}

impl Eq for u128 {
    fn eq(&self, other: &u128) -> bool { ¤u128_eq(*self, *other) }
    fn ne(&self, other: &u128) -> bool { ¤u128_ne(*self, *other) }
}
impl Ord for u128 {
    fn lt(&self, other: &u128) -> bool { ¤u128_lt(*self, *other) }
    fn le(&self, other: &u128) -> bool { ¤u128_le(*self, *other) }
    fn gt(&self, other: &u128) -> bool { ¤u128_gt(*self, *other) }
    fn ge(&self, other: &u128) -> bool { ¤u128_ge(*self, *other) }
}

impl Eq for i128 {
    fn eq(&self, other: &i128) -> bool { ¤i128_eq(*self, *other) }
    fn ne(&self, other: &i128) -> bool { ¤i128_ne(*self, *other) }
}
impl Ord for i128 {
    fn lt(&self, other: &i128) -> bool { ¤i128_lt(*self, *other) }
    fn le(&self, other: &i128) -> bool { ¤i128_le(*self, *other) }
    fn gt(&self, other: &i128) -> bool { ¤i128_gt(*self, *other) }
    fn ge(&self, other: &i128) -> bool { ¤i128_ge(*self, *other) }
}

impl Eq for usize {
    fn eq(&self, other: &usize) -> bool { ¤usize_eq(*self, *other) }
    fn ne(&self, other: &usize) -> bool { ¤usize_ne(*self, *other) }
}
impl Ord for usize {
    fn lt(&self, other: &usize) -> bool { ¤usize_lt(*self, *other) }
    fn le(&self, other: &usize) -> bool { ¤usize_le(*self, *other) }
    fn gt(&self, other: &usize) -> bool { ¤usize_gt(*self, *other) }
    fn ge(&self, other: &usize) -> bool { ¤usize_ge(*self, *other) }
}

impl Eq for isize {
    fn eq(&self, other: &isize) -> bool { ¤isize_eq(*self, *other) }
    fn ne(&self, other: &isize) -> bool { ¤isize_ne(*self, *other) }
}
impl Ord for isize {
    fn lt(&self, other: &isize) -> bool { ¤isize_lt(*self, *other) }
    fn le(&self, other: &isize) -> bool { ¤isize_le(*self, *other) }
    fn gt(&self, other: &isize) -> bool { ¤isize_gt(*self, *other) }
    fn ge(&self, other: &isize) -> bool { ¤isize_ge(*self, *other) }
}

impl Eq for bool {
    fn eq(&self, other: &bool) -> bool { ¤bool_eq(*self, *other) }
    fn ne(&self, other: &bool) -> bool { ¤bool_ne(*self, *other) }
}
