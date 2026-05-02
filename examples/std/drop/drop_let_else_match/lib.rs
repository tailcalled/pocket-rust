// let-else success path with a Drop binding. The Logger inside
// Some is bound to `_l`; on scope end its Drop runs and writes
// 1 to c.
struct Logger { counter: *mut u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe { *self.counter = 1u32; }
    }
}

fn make(c: *mut u32) -> Option<Logger> {
    Option::Some(Logger { counter: c })
}

fn answer() -> u32 {
    let mut c: u32 = 0;
    {
        let Option::Some(_l) = make(&mut c as *mut u32) else { return 99u32 };
    }
    c
}
