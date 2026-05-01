struct Cell<T> { ptr: *mut T }
impl<T> Cell<T> {
    fn new() -> Cell<T> {
        let p: *mut u8 = unsafe { ¤alloc(¤size_of::<T>()) };
        let pt: *mut T = unsafe { ¤cast::<T, u8>(p) };
        Cell { ptr: pt }
    }
    fn put(&mut self, v: T) {
        unsafe { *self.ptr = v; }
    }
    fn get(&self) -> T {
        unsafe { *self.ptr }
    }
}
fn answer() -> u32 {
    let mut c: Cell<u32> = Cell::new();
    c.put(42);
    c.get()
}
