struct Logger { counter: *mut u32 }

impl Drop for Logger {
    fn drop(&mut self) { unsafe { *self.counter = 1; } }
}

fn take(l: Logger) -> u32 { 42 }

fn answer() -> u32 {
    let mut c: u32 = 0;
    let _v: u32 = take(Logger { counter: &mut c as *mut u32 });
    c
}
