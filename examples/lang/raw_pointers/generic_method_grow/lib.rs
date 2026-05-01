struct Buf<T> { ptr: *mut T, len: usize, cap: usize }
impl<T> Buf<T> {
    fn new() -> Buf<T> {
        let p: *mut u8 = 0 as *mut u8;
        let pt: *mut T = unsafe { ¤cast::<T, u8>(p) };
        Buf { ptr: pt, len: 0, cap: 0 }
    }
    fn grow(&mut self) {
        let p: *mut u8 = unsafe { ¤alloc(16) };
        let pt: *mut T = unsafe { ¤cast::<T, u8>(p) };
        self.ptr = pt;
        self.cap = 4;
    }
    fn push(&mut self, v: T) {
        if self.len == self.cap {
            self.grow();
        }
        let elem_size: usize = ¤size_of::<T>();
        let offset: usize = self.len * elem_size;
        unsafe {
            let buf_u8: *mut u8 = ¤cast::<u8, T>(self.ptr);
            let dst_u8: *mut u8 = ¤ptr_usize_add(buf_u8, offset);
            let dst: *mut T = ¤cast::<T, u8>(dst_u8);
            *dst = v;
        }
        self.len = self.len + 1;
    }
    fn first(&self) -> T {
        unsafe { *self.ptr }
    }
}
fn answer() -> u32 {
    let mut b: Buf<u32> = Buf::new();
    b.push(42);
    b.first()
}
