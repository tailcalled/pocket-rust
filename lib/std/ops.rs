pub trait Drop {
    fn drop(&mut self);
}

pub trait Num {
    fn from_i64(x: i64) -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn mul(self, other: Self) -> Self;
    fn div(self, other: Self) -> Self;
    fn rem(self, other: Self) -> Self;
}

impl Num for u8 {
    fn from_i64(x: i64) -> u8 { x as u8 }
    fn add(self, other: u8) -> u8 { ¤u8_add(self, other) }
    fn sub(self, other: u8) -> u8 { ¤u8_sub(self, other) }
    fn mul(self, other: u8) -> u8 { ¤u8_mul(self, other) }
    fn div(self, other: u8) -> u8 { ¤u8_div(self, other) }
    fn rem(self, other: u8) -> u8 { ¤u8_rem(self, other) }
}
impl Num for i8 {
    fn from_i64(x: i64) -> i8 { x as i8 }
    fn add(self, other: i8) -> i8 { ¤i8_add(self, other) }
    fn sub(self, other: i8) -> i8 { ¤i8_sub(self, other) }
    fn mul(self, other: i8) -> i8 { ¤i8_mul(self, other) }
    fn div(self, other: i8) -> i8 { ¤i8_div(self, other) }
    fn rem(self, other: i8) -> i8 { ¤i8_rem(self, other) }
}
impl Num for u16 {
    fn from_i64(x: i64) -> u16 { x as u16 }
    fn add(self, other: u16) -> u16 { ¤u16_add(self, other) }
    fn sub(self, other: u16) -> u16 { ¤u16_sub(self, other) }
    fn mul(self, other: u16) -> u16 { ¤u16_mul(self, other) }
    fn div(self, other: u16) -> u16 { ¤u16_div(self, other) }
    fn rem(self, other: u16) -> u16 { ¤u16_rem(self, other) }
}
impl Num for i16 {
    fn from_i64(x: i64) -> i16 { x as i16 }
    fn add(self, other: i16) -> i16 { ¤i16_add(self, other) }
    fn sub(self, other: i16) -> i16 { ¤i16_sub(self, other) }
    fn mul(self, other: i16) -> i16 { ¤i16_mul(self, other) }
    fn div(self, other: i16) -> i16 { ¤i16_div(self, other) }
    fn rem(self, other: i16) -> i16 { ¤i16_rem(self, other) }
}
impl Num for u32 {
    fn from_i64(x: i64) -> u32 { x as u32 }
    fn add(self, other: u32) -> u32 { ¤u32_add(self, other) }
    fn sub(self, other: u32) -> u32 { ¤u32_sub(self, other) }
    fn mul(self, other: u32) -> u32 { ¤u32_mul(self, other) }
    fn div(self, other: u32) -> u32 { ¤u32_div(self, other) }
    fn rem(self, other: u32) -> u32 { ¤u32_rem(self, other) }
}
impl Num for i32 {
    fn from_i64(x: i64) -> i32 { x as i32 }
    fn add(self, other: i32) -> i32 { ¤i32_add(self, other) }
    fn sub(self, other: i32) -> i32 { ¤i32_sub(self, other) }
    fn mul(self, other: i32) -> i32 { ¤i32_mul(self, other) }
    fn div(self, other: i32) -> i32 { ¤i32_div(self, other) }
    fn rem(self, other: i32) -> i32 { ¤i32_rem(self, other) }
}
impl Num for u64 {
    fn from_i64(x: i64) -> u64 { x as u64 }
    fn add(self, other: u64) -> u64 { ¤u64_add(self, other) }
    fn sub(self, other: u64) -> u64 { ¤u64_sub(self, other) }
    fn mul(self, other: u64) -> u64 { ¤u64_mul(self, other) }
    fn div(self, other: u64) -> u64 { ¤u64_div(self, other) }
    fn rem(self, other: u64) -> u64 { ¤u64_rem(self, other) }
}
impl Num for i64 {
    fn from_i64(x: i64) -> i64 { x }
    fn add(self, other: i64) -> i64 { ¤i64_add(self, other) }
    fn sub(self, other: i64) -> i64 { ¤i64_sub(self, other) }
    fn mul(self, other: i64) -> i64 { ¤i64_mul(self, other) }
    fn div(self, other: i64) -> i64 { ¤i64_div(self, other) }
    fn rem(self, other: i64) -> i64 { ¤i64_rem(self, other) }
}
impl Num for u128 {
    fn from_i64(x: i64) -> u128 { x as u128 }
    fn add(self, other: u128) -> u128 { ¤u128_add(self, other) }
    fn sub(self, other: u128) -> u128 { ¤u128_sub(self, other) }
    fn mul(self, other: u128) -> u128 { ¤u128_mul(self, other) }
    fn div(self, other: u128) -> u128 { ¤u128_div(self, other) }
    fn rem(self, other: u128) -> u128 { ¤u128_rem(self, other) }
}
impl Num for i128 {
    fn from_i64(x: i64) -> i128 { x as i128 }
    fn add(self, other: i128) -> i128 { ¤i128_add(self, other) }
    fn sub(self, other: i128) -> i128 { ¤i128_sub(self, other) }
    fn mul(self, other: i128) -> i128 { ¤i128_mul(self, other) }
    fn div(self, other: i128) -> i128 { ¤i128_div(self, other) }
    fn rem(self, other: i128) -> i128 { ¤i128_rem(self, other) }
}
impl Num for usize {
    fn from_i64(x: i64) -> usize { x as usize }
    fn add(self, other: usize) -> usize { ¤usize_add(self, other) }
    fn sub(self, other: usize) -> usize { ¤usize_sub(self, other) }
    fn mul(self, other: usize) -> usize { ¤usize_mul(self, other) }
    fn div(self, other: usize) -> usize { ¤usize_div(self, other) }
    fn rem(self, other: usize) -> usize { ¤usize_rem(self, other) }
}
impl Num for isize {
    fn from_i64(x: i64) -> isize { x as isize }
    fn add(self, other: isize) -> isize { ¤isize_add(self, other) }
    fn sub(self, other: isize) -> isize { ¤isize_sub(self, other) }
    fn mul(self, other: isize) -> isize { ¤isize_mul(self, other) }
    fn div(self, other: isize) -> isize { ¤isize_div(self, other) }
    fn rem(self, other: isize) -> isize { ¤isize_rem(self, other) }
}
