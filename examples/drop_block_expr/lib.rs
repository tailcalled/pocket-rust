struct Logger { counter: *mut u32 }

impl Drop for Logger {
    fn drop(&mut self) { unsafe { *self.counter = 7; } }
}

fn answer() -> u32 {
    let mut c: u32 = 0;
    let v: u32 = {
        let _l: Logger = Logger { counter: &mut c as *mut u32 };
        42
    };
    c
}
