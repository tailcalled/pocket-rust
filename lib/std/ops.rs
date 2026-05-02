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

// Smart-pointer dereference. `*box` for a `box: Box<T>` desugars at
// typeck/codegen to `*Deref::deref(&box)` (where the call returns
// `&T` and the outer deref reads the T value out). `&*box` and
// `&mut *box` similarly route through `Deref::deref` / `DerefMut::
// deref_mut` to produce the inner ref directly. Auto-deref for
// method dispatch (`box.method()` → `(*box).method()` automatically)
// is a follow-up — for now use the explicit `*` form.
pub trait Deref {
    type Target;
    fn deref(&self) -> &Self::Target;
}
pub trait DerefMut: Deref {
    fn deref_mut(&mut self) -> &mut Self::Target;
}

// Rust-style operator-overloading traits. Each binary op is one trait
// with a `Rhs = Self` default and an `Output` assoc type, so users
// can write asymmetric operations (e.g. `Vec3 * f32`) without
// committing to `Self == Rhs == Output`. The parser desugars
// `a + b` to `a.add(b)` (and similarly sub/mul/div/rem); `-x` to
// `x.neg()`.
pub trait Add<Rhs = Self> {
    type Output;
    fn add(self, other: Rhs) -> Self::Output;
}
pub trait Sub<Rhs = Self> {
    type Output;
    fn sub(self, other: Rhs) -> Self::Output;
}
pub trait Mul<Rhs = Self> {
    type Output;
    fn mul(self, other: Rhs) -> Self::Output;
}
pub trait Div<Rhs = Self> {
    type Output;
    fn div(self, other: Rhs) -> Self::Output;
}
pub trait Rem<Rhs = Self> {
    type Output;
    fn rem(self, other: Rhs) -> Self::Output;
}
pub trait Neg {
    type Output;
    fn neg(self) -> Self::Output;
}

// Compound-assignment traits (`a += b` desugars to
// `AddAssign::add_assign(&mut a, b)` and similarly for the other
// four). The receiver is `&mut self` and the method returns no value;
// these can be implemented separately from `Add` / `Sub` / etc. when
// in-place mutation is more efficient or has different semantics.
pub trait AddAssign<Rhs = Self> {
    fn add_assign(&mut self, other: Rhs);
}
pub trait SubAssign<Rhs = Self> {
    fn sub_assign(&mut self, other: Rhs);
}
pub trait MulAssign<Rhs = Self> {
    fn mul_assign(&mut self, other: Rhs);
}
pub trait DivAssign<Rhs = Self> {
    fn div_assign(&mut self, other: Rhs);
}
pub trait RemAssign<Rhs = Self> {
    fn rem_assign(&mut self, other: Rhs);
}

// Primitive arithmetic impls. Each int kind T gets `impl Add<T> for
// T` (with `Output = T`), and so on — same-Self only. Cross-kind
// arithmetic (e.g. `u32 + u8`) requires an `as` cast first; this
// matches Rust's behavior and avoids a combinatorial explosion of
// impls.
impl Add for u8 { type Output = u8; fn add(self, other: u8) -> u8 { ¤u8_add(self, other) } }
impl Sub for u8 { type Output = u8; fn sub(self, other: u8) -> u8 { ¤u8_sub(self, other) } }
impl Mul for u8 { type Output = u8; fn mul(self, other: u8) -> u8 { ¤u8_mul(self, other) } }
impl Div for u8 { type Output = u8; fn div(self, other: u8) -> u8 { ¤u8_div(self, other) } }
impl Rem for u8 { type Output = u8; fn rem(self, other: u8) -> u8 { ¤u8_rem(self, other) } }
impl Neg for u8 { type Output = u8; fn neg(self) -> u8 { ¤u8_sub(0, self) } }

impl Add for i8 { type Output = i8; fn add(self, other: i8) -> i8 { ¤i8_add(self, other) } }
impl Sub for i8 { type Output = i8; fn sub(self, other: i8) -> i8 { ¤i8_sub(self, other) } }
impl Mul for i8 { type Output = i8; fn mul(self, other: i8) -> i8 { ¤i8_mul(self, other) } }
impl Div for i8 { type Output = i8; fn div(self, other: i8) -> i8 { ¤i8_div(self, other) } }
impl Rem for i8 { type Output = i8; fn rem(self, other: i8) -> i8 { ¤i8_rem(self, other) } }
impl Neg for i8 { type Output = i8; fn neg(self) -> i8 { ¤i8_sub(0, self) } }

impl Add for u16 { type Output = u16; fn add(self, other: u16) -> u16 { ¤u16_add(self, other) } }
impl Sub for u16 { type Output = u16; fn sub(self, other: u16) -> u16 { ¤u16_sub(self, other) } }
impl Mul for u16 { type Output = u16; fn mul(self, other: u16) -> u16 { ¤u16_mul(self, other) } }
impl Div for u16 { type Output = u16; fn div(self, other: u16) -> u16 { ¤u16_div(self, other) } }
impl Rem for u16 { type Output = u16; fn rem(self, other: u16) -> u16 { ¤u16_rem(self, other) } }
impl Neg for u16 { type Output = u16; fn neg(self) -> u16 { ¤u16_sub(0, self) } }

impl Add for i16 { type Output = i16; fn add(self, other: i16) -> i16 { ¤i16_add(self, other) } }
impl Sub for i16 { type Output = i16; fn sub(self, other: i16) -> i16 { ¤i16_sub(self, other) } }
impl Mul for i16 { type Output = i16; fn mul(self, other: i16) -> i16 { ¤i16_mul(self, other) } }
impl Div for i16 { type Output = i16; fn div(self, other: i16) -> i16 { ¤i16_div(self, other) } }
impl Rem for i16 { type Output = i16; fn rem(self, other: i16) -> i16 { ¤i16_rem(self, other) } }
impl Neg for i16 { type Output = i16; fn neg(self) -> i16 { ¤i16_sub(0, self) } }

impl Add for u32 { type Output = u32; fn add(self, other: u32) -> u32 { ¤u32_add(self, other) } }
impl Sub for u32 { type Output = u32; fn sub(self, other: u32) -> u32 { ¤u32_sub(self, other) } }
impl Mul for u32 { type Output = u32; fn mul(self, other: u32) -> u32 { ¤u32_mul(self, other) } }
impl Div for u32 { type Output = u32; fn div(self, other: u32) -> u32 { ¤u32_div(self, other) } }
impl Rem for u32 { type Output = u32; fn rem(self, other: u32) -> u32 { ¤u32_rem(self, other) } }
impl Neg for u32 { type Output = u32; fn neg(self) -> u32 { ¤u32_sub(0, self) } }

impl Add for i32 { type Output = i32; fn add(self, other: i32) -> i32 { ¤i32_add(self, other) } }
impl Sub for i32 { type Output = i32; fn sub(self, other: i32) -> i32 { ¤i32_sub(self, other) } }
impl Mul for i32 { type Output = i32; fn mul(self, other: i32) -> i32 { ¤i32_mul(self, other) } }
impl Div for i32 { type Output = i32; fn div(self, other: i32) -> i32 { ¤i32_div(self, other) } }
impl Rem for i32 { type Output = i32; fn rem(self, other: i32) -> i32 { ¤i32_rem(self, other) } }
impl Neg for i32 { type Output = i32; fn neg(self) -> i32 { ¤i32_sub(0, self) } }

impl Add for u64 { type Output = u64; fn add(self, other: u64) -> u64 { ¤u64_add(self, other) } }
impl Sub for u64 { type Output = u64; fn sub(self, other: u64) -> u64 { ¤u64_sub(self, other) } }
impl Mul for u64 { type Output = u64; fn mul(self, other: u64) -> u64 { ¤u64_mul(self, other) } }
impl Div for u64 { type Output = u64; fn div(self, other: u64) -> u64 { ¤u64_div(self, other) } }
impl Rem for u64 { type Output = u64; fn rem(self, other: u64) -> u64 { ¤u64_rem(self, other) } }
impl Neg for u64 { type Output = u64; fn neg(self) -> u64 { ¤u64_sub(0, self) } }

impl Add for i64 { type Output = i64; fn add(self, other: i64) -> i64 { ¤i64_add(self, other) } }
impl Sub for i64 { type Output = i64; fn sub(self, other: i64) -> i64 { ¤i64_sub(self, other) } }
impl Mul for i64 { type Output = i64; fn mul(self, other: i64) -> i64 { ¤i64_mul(self, other) } }
impl Div for i64 { type Output = i64; fn div(self, other: i64) -> i64 { ¤i64_div(self, other) } }
impl Rem for i64 { type Output = i64; fn rem(self, other: i64) -> i64 { ¤i64_rem(self, other) } }
impl Neg for i64 { type Output = i64; fn neg(self) -> i64 { ¤i64_sub(0, self) } }

impl Add for u128 { type Output = u128; fn add(self, other: u128) -> u128 { ¤u128_add(self, other) } }
impl Sub for u128 { type Output = u128; fn sub(self, other: u128) -> u128 { ¤u128_sub(self, other) } }
impl Mul for u128 { type Output = u128; fn mul(self, other: u128) -> u128 { ¤u128_mul(self, other) } }
impl Div for u128 { type Output = u128; fn div(self, other: u128) -> u128 { ¤u128_div(self, other) } }
impl Rem for u128 { type Output = u128; fn rem(self, other: u128) -> u128 { ¤u128_rem(self, other) } }
impl Neg for u128 { type Output = u128; fn neg(self) -> u128 { ¤u128_sub(0, self) } }

impl Add for i128 { type Output = i128; fn add(self, other: i128) -> i128 { ¤i128_add(self, other) } }
impl Sub for i128 { type Output = i128; fn sub(self, other: i128) -> i128 { ¤i128_sub(self, other) } }
impl Mul for i128 { type Output = i128; fn mul(self, other: i128) -> i128 { ¤i128_mul(self, other) } }
impl Div for i128 { type Output = i128; fn div(self, other: i128) -> i128 { ¤i128_div(self, other) } }
impl Rem for i128 { type Output = i128; fn rem(self, other: i128) -> i128 { ¤i128_rem(self, other) } }
impl Neg for i128 { type Output = i128; fn neg(self) -> i128 { ¤i128_sub(0, self) } }

impl Add for usize { type Output = usize; fn add(self, other: usize) -> usize { ¤usize_add(self, other) } }
impl Sub for usize { type Output = usize; fn sub(self, other: usize) -> usize { ¤usize_sub(self, other) } }
impl Mul for usize { type Output = usize; fn mul(self, other: usize) -> usize { ¤usize_mul(self, other) } }
impl Div for usize { type Output = usize; fn div(self, other: usize) -> usize { ¤usize_div(self, other) } }
impl Rem for usize { type Output = usize; fn rem(self, other: usize) -> usize { ¤usize_rem(self, other) } }
impl Neg for usize { type Output = usize; fn neg(self) -> usize { ¤usize_sub(0, self) } }

impl Add for isize { type Output = isize; fn add(self, other: isize) -> isize { ¤isize_add(self, other) } }
impl Sub for isize { type Output = isize; fn sub(self, other: isize) -> isize { ¤isize_sub(self, other) } }
impl Mul for isize { type Output = isize; fn mul(self, other: isize) -> isize { ¤isize_mul(self, other) } }
impl Div for isize { type Output = isize; fn div(self, other: isize) -> isize { ¤isize_div(self, other) } }
impl Rem for isize { type Output = isize; fn rem(self, other: isize) -> isize { ¤isize_rem(self, other) } }
impl Neg for isize { type Output = isize; fn neg(self) -> isize { ¤isize_sub(0, self) } }

// Primitive compound-assignment impls. Each `*_assign` body
// dereferences `self` (a `&mut T`), recomputes via the matching
// `¤T_op`, and stores back through the reference.
impl AddAssign for u8 { fn add_assign(&mut self, other: u8) { *self = ¤u8_add(*self, other); } }
impl SubAssign for u8 { fn sub_assign(&mut self, other: u8) { *self = ¤u8_sub(*self, other); } }
impl MulAssign for u8 { fn mul_assign(&mut self, other: u8) { *self = ¤u8_mul(*self, other); } }
impl DivAssign for u8 { fn div_assign(&mut self, other: u8) { *self = ¤u8_div(*self, other); } }
impl RemAssign for u8 { fn rem_assign(&mut self, other: u8) { *self = ¤u8_rem(*self, other); } }

impl AddAssign for i8 { fn add_assign(&mut self, other: i8) { *self = ¤i8_add(*self, other); } }
impl SubAssign for i8 { fn sub_assign(&mut self, other: i8) { *self = ¤i8_sub(*self, other); } }
impl MulAssign for i8 { fn mul_assign(&mut self, other: i8) { *self = ¤i8_mul(*self, other); } }
impl DivAssign for i8 { fn div_assign(&mut self, other: i8) { *self = ¤i8_div(*self, other); } }
impl RemAssign for i8 { fn rem_assign(&mut self, other: i8) { *self = ¤i8_rem(*self, other); } }

impl AddAssign for u16 { fn add_assign(&mut self, other: u16) { *self = ¤u16_add(*self, other); } }
impl SubAssign for u16 { fn sub_assign(&mut self, other: u16) { *self = ¤u16_sub(*self, other); } }
impl MulAssign for u16 { fn mul_assign(&mut self, other: u16) { *self = ¤u16_mul(*self, other); } }
impl DivAssign for u16 { fn div_assign(&mut self, other: u16) { *self = ¤u16_div(*self, other); } }
impl RemAssign for u16 { fn rem_assign(&mut self, other: u16) { *self = ¤u16_rem(*self, other); } }

impl AddAssign for i16 { fn add_assign(&mut self, other: i16) { *self = ¤i16_add(*self, other); } }
impl SubAssign for i16 { fn sub_assign(&mut self, other: i16) { *self = ¤i16_sub(*self, other); } }
impl MulAssign for i16 { fn mul_assign(&mut self, other: i16) { *self = ¤i16_mul(*self, other); } }
impl DivAssign for i16 { fn div_assign(&mut self, other: i16) { *self = ¤i16_div(*self, other); } }
impl RemAssign for i16 { fn rem_assign(&mut self, other: i16) { *self = ¤i16_rem(*self, other); } }

impl AddAssign for u32 { fn add_assign(&mut self, other: u32) { *self = ¤u32_add(*self, other); } }
impl SubAssign for u32 { fn sub_assign(&mut self, other: u32) { *self = ¤u32_sub(*self, other); } }
impl MulAssign for u32 { fn mul_assign(&mut self, other: u32) { *self = ¤u32_mul(*self, other); } }
impl DivAssign for u32 { fn div_assign(&mut self, other: u32) { *self = ¤u32_div(*self, other); } }
impl RemAssign for u32 { fn rem_assign(&mut self, other: u32) { *self = ¤u32_rem(*self, other); } }

impl AddAssign for i32 { fn add_assign(&mut self, other: i32) { *self = ¤i32_add(*self, other); } }
impl SubAssign for i32 { fn sub_assign(&mut self, other: i32) { *self = ¤i32_sub(*self, other); } }
impl MulAssign for i32 { fn mul_assign(&mut self, other: i32) { *self = ¤i32_mul(*self, other); } }
impl DivAssign for i32 { fn div_assign(&mut self, other: i32) { *self = ¤i32_div(*self, other); } }
impl RemAssign for i32 { fn rem_assign(&mut self, other: i32) { *self = ¤i32_rem(*self, other); } }

impl AddAssign for u64 { fn add_assign(&mut self, other: u64) { *self = ¤u64_add(*self, other); } }
impl SubAssign for u64 { fn sub_assign(&mut self, other: u64) { *self = ¤u64_sub(*self, other); } }
impl MulAssign for u64 { fn mul_assign(&mut self, other: u64) { *self = ¤u64_mul(*self, other); } }
impl DivAssign for u64 { fn div_assign(&mut self, other: u64) { *self = ¤u64_div(*self, other); } }
impl RemAssign for u64 { fn rem_assign(&mut self, other: u64) { *self = ¤u64_rem(*self, other); } }

impl AddAssign for i64 { fn add_assign(&mut self, other: i64) { *self = ¤i64_add(*self, other); } }
impl SubAssign for i64 { fn sub_assign(&mut self, other: i64) { *self = ¤i64_sub(*self, other); } }
impl MulAssign for i64 { fn mul_assign(&mut self, other: i64) { *self = ¤i64_mul(*self, other); } }
impl DivAssign for i64 { fn div_assign(&mut self, other: i64) { *self = ¤i64_div(*self, other); } }
impl RemAssign for i64 { fn rem_assign(&mut self, other: i64) { *self = ¤i64_rem(*self, other); } }

impl AddAssign for u128 { fn add_assign(&mut self, other: u128) { *self = ¤u128_add(*self, other); } }
impl SubAssign for u128 { fn sub_assign(&mut self, other: u128) { *self = ¤u128_sub(*self, other); } }
impl MulAssign for u128 { fn mul_assign(&mut self, other: u128) { *self = ¤u128_mul(*self, other); } }
impl DivAssign for u128 { fn div_assign(&mut self, other: u128) { *self = ¤u128_div(*self, other); } }
impl RemAssign for u128 { fn rem_assign(&mut self, other: u128) { *self = ¤u128_rem(*self, other); } }

impl AddAssign for i128 { fn add_assign(&mut self, other: i128) { *self = ¤i128_add(*self, other); } }
impl SubAssign for i128 { fn sub_assign(&mut self, other: i128) { *self = ¤i128_sub(*self, other); } }
impl MulAssign for i128 { fn mul_assign(&mut self, other: i128) { *self = ¤i128_mul(*self, other); } }
impl DivAssign for i128 { fn div_assign(&mut self, other: i128) { *self = ¤i128_div(*self, other); } }
impl RemAssign for i128 { fn rem_assign(&mut self, other: i128) { *self = ¤i128_rem(*self, other); } }

impl AddAssign for usize { fn add_assign(&mut self, other: usize) { *self = ¤usize_add(*self, other); } }
impl SubAssign for usize { fn sub_assign(&mut self, other: usize) { *self = ¤usize_sub(*self, other); } }
impl MulAssign for usize { fn mul_assign(&mut self, other: usize) { *self = ¤usize_mul(*self, other); } }
impl DivAssign for usize { fn div_assign(&mut self, other: usize) { *self = ¤usize_div(*self, other); } }
impl RemAssign for usize { fn rem_assign(&mut self, other: usize) { *self = ¤usize_rem(*self, other); } }

impl AddAssign for isize { fn add_assign(&mut self, other: isize) { *self = ¤isize_add(*self, other); } }
impl SubAssign for isize { fn sub_assign(&mut self, other: isize) { *self = ¤isize_sub(*self, other); } }
impl MulAssign for isize { fn mul_assign(&mut self, other: isize) { *self = ¤isize_mul(*self, other); } }
impl DivAssign for isize { fn div_assign(&mut self, other: isize) { *self = ¤isize_div(*self, other); } }
impl RemAssign for isize { fn rem_assign(&mut self, other: isize) { *self = ¤isize_rem(*self, other); } }
