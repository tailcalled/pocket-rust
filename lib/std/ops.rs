pub trait Drop {
    fn drop(&mut self);
}

pub trait Num {
    fn from_i64(x: i64) -> Self;
}

impl Num for u8 { fn from_i64(x: i64) -> u8 { x as u8 } }
impl Num for i8 { fn from_i64(x: i64) -> i8 { x as i8 } }
impl Num for u16 { fn from_i64(x: i64) -> u16 { x as u16 } }
impl Num for i16 { fn from_i64(x: i64) -> i16 { x as i16 } }
impl Num for u32 { fn from_i64(x: i64) -> u32 { x as u32 } }
impl Num for i32 { fn from_i64(x: i64) -> i32 { x as i32 } }
impl Num for u64 { fn from_i64(x: i64) -> u64 { x as u64 } }
impl Num for i64 { fn from_i64(x: i64) -> i64 { x } }
impl Num for u128 { fn from_i64(x: i64) -> u128 { x as u128 } }
impl Num for i128 { fn from_i64(x: i64) -> i128 { x as i128 } }
impl Num for usize { fn from_i64(x: i64) -> usize { x as usize } }
impl Num for isize { fn from_i64(x: i64) -> isize { x as isize } }
