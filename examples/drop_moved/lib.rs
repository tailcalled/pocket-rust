struct Logger { counter: *mut u32, sink: *mut u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe {
            *self.sink = *self.counter;
            *self.counter = 1;
        }
    }
}

fn answer() -> u32 {
    let mut c: u32 = 0;
    let mut s: u32 = 99;
    let _v: u32 = {
        let l: Logger = Logger {
            counter: &mut c as *mut u32,
            sink: &mut s as *mut u32,
        };
        let _y: Logger = l;
        42
    };
    s
}
