// let-else mismatched: pattern doesn't match (None), so the
// else block runs and returns 1 from the function. The Some
// branch's binding (_l) never enters scope.
struct Logger { counter: *mut u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe { *self.counter = 99u32; }
    }
}

fn make(_c: *mut u32) -> Option<Logger> {
    Option::None
}

fn answer() -> u32 {
    let mut c: u32 = 0;
    let Option::Some(_l) = make(&mut c as *mut u32) else { return 1u32 };
    0u32
}
