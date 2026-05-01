// User-side mini-Vec: confirms that a generic struct with `*mut T` +
// length, `&mut self` push/grow with `&mut self`-to-`&mut self` reborrow,
// and `Option<T>` pop work end-to-end without invoking the stdlib's
// `Vec` (which exercises the same machinery from a library crate).
struct MyVec<T> { ptr: *mut T, len: usize, cap: usize }
impl<T> MyVec<T> {
    fn new() -> MyVec<T> {
        let null_u8: *mut u8 = 0 as *mut u8;
        let null_t: *mut T = unsafe { ¤cast::<T, u8>(null_u8) };
        MyVec { ptr: null_t, len: 0, cap: 0 }
    }
    fn push(&mut self, value: T) {
        if self.cap == 0 {
            let p: *mut u8 = unsafe { ¤alloc(16) };
            self.ptr = unsafe { ¤cast::<T, u8>(p) };
            self.cap = 4;
        }
        unsafe { *self.ptr = value; }
        self.len = self.len + 1;
    }
    fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            Option::None
        } else {
            self.len = self.len - 1;
            unsafe { Option::Some(*self.ptr) }
        }
    }
}
fn answer() -> u32 {
    let mut v: MyVec<u32> = MyVec::new();
    v.push(42);
    match v.pop() {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
