struct Logger { counter: *mut u32, sink: *mut u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe {
            *self.sink = *self.counter;
            *self.counter = 1;
        }
    }
}

fn run(b: bool, c: *mut u32, s: *mut u32) -> u32 {
    let l: Logger = Logger { counter: c, sink: s };
    if b { 7 } else { 8 }
}

fn answer() -> u32 {
    let mut c: u32 = 5;
    let mut s: u32 = 99;
    let _v: u32 = run(true, &mut c as *mut u32, &mut s as *mut u32);
    s
}
