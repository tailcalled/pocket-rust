pub trait Drop {
    fn drop(&mut self);
}

// Indexing read: `arr[idx]` desugars at typeck/codegen to
// `*Index::index(&arr, idx)`. The associated `Output` lets each impl
// pick what it returns (typically the element type).
//
// Pocket-rust currently hardcodes the index type to `usize` (no
// generic-trait support yet — `trait Index<Idx>` requires generic
// trait parameters which the parser doesn't accept). When generic
// traits land, the canonical signature is `trait Index<Idx> { type
// Output; fn index(&self, idx: Idx) -> &Self::Output; }` and impls
// are restated with `usize`.
pub trait Index {
    type Output;
    fn index(&self, idx: usize) -> &Self::Output;
}

// Indexing write: `arr[idx] = …` and `&mut arr[idx]` desugar to
// `IndexMut::index_mut(&mut arr, idx)`. `IndexMut: Index` so the
// shared-ref `index` is always available too.
pub trait IndexMut: Index {
    fn index_mut(&mut self, idx: usize) -> &mut Self::Output;
}

// Additive structure (Haskell-style algebraic decomposition): a type
// with a `zero`, addition/subtraction, and negation. The unary-minus
// operator `-x` desugars to `x.neg()`, dispatched through `VecSpace`.
// Every `Num` is a `VecSpace`; `VecSpace` alone covers things like
// vectors that have addition but not multiplication or literals.
pub trait VecSpace {
    fn zero() -> Self;
    fn neg(self) -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
}

// Numeric scalars: integer literals dispatch through `from_i64`, and
// multiplication/division/remainder live here. `Num: VecSpace` means
// every `Num` type already has `add`/`sub`/`neg`/`zero` available.
pub trait Num: VecSpace {
    fn from_i64(x: i64) -> Self;
    fn mul(self, other: Self) -> Self;
    fn div(self, other: Self) -> Self;
    fn rem(self, other: Self) -> Self;
}

impl VecSpace for u8 {
    fn zero() -> u8 { 0 }
    fn neg(self) -> u8 { ¤u8_sub(0, self) }
    fn add(self, other: u8) -> u8 { ¤u8_add(self, other) }
    fn sub(self, other: u8) -> u8 { ¤u8_sub(self, other) }
}
impl Num for u8 {
    fn from_i64(x: i64) -> u8 { x as u8 }
    fn mul(self, other: u8) -> u8 { ¤u8_mul(self, other) }
    fn div(self, other: u8) -> u8 { ¤u8_div(self, other) }
    fn rem(self, other: u8) -> u8 { ¤u8_rem(self, other) }
}
impl VecSpace for i8 {
    fn zero() -> i8 { 0 }
    fn neg(self) -> i8 { ¤i8_sub(0, self) }
    fn add(self, other: i8) -> i8 { ¤i8_add(self, other) }
    fn sub(self, other: i8) -> i8 { ¤i8_sub(self, other) }
}
impl Num for i8 {
    fn from_i64(x: i64) -> i8 { x as i8 }
    fn mul(self, other: i8) -> i8 { ¤i8_mul(self, other) }
    fn div(self, other: i8) -> i8 { ¤i8_div(self, other) }
    fn rem(self, other: i8) -> i8 { ¤i8_rem(self, other) }
}
impl VecSpace for u16 {
    fn zero() -> u16 { 0 }
    fn neg(self) -> u16 { ¤u16_sub(0, self) }
    fn add(self, other: u16) -> u16 { ¤u16_add(self, other) }
    fn sub(self, other: u16) -> u16 { ¤u16_sub(self, other) }
}
impl Num for u16 {
    fn from_i64(x: i64) -> u16 { x as u16 }
    fn mul(self, other: u16) -> u16 { ¤u16_mul(self, other) }
    fn div(self, other: u16) -> u16 { ¤u16_div(self, other) }
    fn rem(self, other: u16) -> u16 { ¤u16_rem(self, other) }
}
impl VecSpace for i16 {
    fn zero() -> i16 { 0 }
    fn neg(self) -> i16 { ¤i16_sub(0, self) }
    fn add(self, other: i16) -> i16 { ¤i16_add(self, other) }
    fn sub(self, other: i16) -> i16 { ¤i16_sub(self, other) }
}
impl Num for i16 {
    fn from_i64(x: i64) -> i16 { x as i16 }
    fn mul(self, other: i16) -> i16 { ¤i16_mul(self, other) }
    fn div(self, other: i16) -> i16 { ¤i16_div(self, other) }
    fn rem(self, other: i16) -> i16 { ¤i16_rem(self, other) }
}
impl VecSpace for u32 {
    fn zero() -> u32 { 0 }
    fn neg(self) -> u32 { ¤u32_sub(0, self) }
    fn add(self, other: u32) -> u32 { ¤u32_add(self, other) }
    fn sub(self, other: u32) -> u32 { ¤u32_sub(self, other) }
}
impl Num for u32 {
    fn from_i64(x: i64) -> u32 { x as u32 }
    fn mul(self, other: u32) -> u32 { ¤u32_mul(self, other) }
    fn div(self, other: u32) -> u32 { ¤u32_div(self, other) }
    fn rem(self, other: u32) -> u32 { ¤u32_rem(self, other) }
}
impl VecSpace for i32 {
    fn zero() -> i32 { 0 }
    fn neg(self) -> i32 { ¤i32_sub(0, self) }
    fn add(self, other: i32) -> i32 { ¤i32_add(self, other) }
    fn sub(self, other: i32) -> i32 { ¤i32_sub(self, other) }
}
impl Num for i32 {
    fn from_i64(x: i64) -> i32 { x as i32 }
    fn mul(self, other: i32) -> i32 { ¤i32_mul(self, other) }
    fn div(self, other: i32) -> i32 { ¤i32_div(self, other) }
    fn rem(self, other: i32) -> i32 { ¤i32_rem(self, other) }
}
impl VecSpace for u64 {
    fn zero() -> u64 { 0 }
    fn neg(self) -> u64 { ¤u64_sub(0, self) }
    fn add(self, other: u64) -> u64 { ¤u64_add(self, other) }
    fn sub(self, other: u64) -> u64 { ¤u64_sub(self, other) }
}
impl Num for u64 {
    fn from_i64(x: i64) -> u64 { x as u64 }
    fn mul(self, other: u64) -> u64 { ¤u64_mul(self, other) }
    fn div(self, other: u64) -> u64 { ¤u64_div(self, other) }
    fn rem(self, other: u64) -> u64 { ¤u64_rem(self, other) }
}
impl VecSpace for i64 {
    fn zero() -> i64 { 0 }
    fn neg(self) -> i64 { ¤i64_sub(0, self) }
    fn add(self, other: i64) -> i64 { ¤i64_add(self, other) }
    fn sub(self, other: i64) -> i64 { ¤i64_sub(self, other) }
}
impl Num for i64 {
    fn from_i64(x: i64) -> i64 { x }
    fn mul(self, other: i64) -> i64 { ¤i64_mul(self, other) }
    fn div(self, other: i64) -> i64 { ¤i64_div(self, other) }
    fn rem(self, other: i64) -> i64 { ¤i64_rem(self, other) }
}
impl VecSpace for u128 {
    fn zero() -> u128 { 0 }
    fn neg(self) -> u128 { ¤u128_sub(0, self) }
    fn add(self, other: u128) -> u128 { ¤u128_add(self, other) }
    fn sub(self, other: u128) -> u128 { ¤u128_sub(self, other) }
}
impl Num for u128 {
    fn from_i64(x: i64) -> u128 { x as u128 }
    fn mul(self, other: u128) -> u128 { ¤u128_mul(self, other) }
    fn div(self, other: u128) -> u128 { ¤u128_div(self, other) }
    fn rem(self, other: u128) -> u128 { ¤u128_rem(self, other) }
}
impl VecSpace for i128 {
    fn zero() -> i128 { 0 }
    fn neg(self) -> i128 { ¤i128_sub(0, self) }
    fn add(self, other: i128) -> i128 { ¤i128_add(self, other) }
    fn sub(self, other: i128) -> i128 { ¤i128_sub(self, other) }
}
impl Num for i128 {
    fn from_i64(x: i64) -> i128 { x as i128 }
    fn mul(self, other: i128) -> i128 { ¤i128_mul(self, other) }
    fn div(self, other: i128) -> i128 { ¤i128_div(self, other) }
    fn rem(self, other: i128) -> i128 { ¤i128_rem(self, other) }
}
impl VecSpace for usize {
    fn zero() -> usize { 0 }
    fn neg(self) -> usize { ¤usize_sub(0, self) }
    fn add(self, other: usize) -> usize { ¤usize_add(self, other) }
    fn sub(self, other: usize) -> usize { ¤usize_sub(self, other) }
}
impl Num for usize {
    fn from_i64(x: i64) -> usize { x as usize }
    fn mul(self, other: usize) -> usize { ¤usize_mul(self, other) }
    fn div(self, other: usize) -> usize { ¤usize_div(self, other) }
    fn rem(self, other: usize) -> usize { ¤usize_rem(self, other) }
}
impl VecSpace for isize {
    fn zero() -> isize { 0 }
    fn neg(self) -> isize { ¤isize_sub(0, self) }
    fn add(self, other: isize) -> isize { ¤isize_add(self, other) }
    fn sub(self, other: isize) -> isize { ¤isize_sub(self, other) }
}
impl Num for isize {
    fn from_i64(x: i64) -> isize { x as isize }
    fn mul(self, other: isize) -> isize { ¤isize_mul(self, other) }
    fn div(self, other: isize) -> isize { ¤isize_div(self, other) }
    fn rem(self, other: isize) -> isize { ¤isize_rem(self, other) }
}
