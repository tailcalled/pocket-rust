// Tuple destructure of Drop values: each binding ends up owning
// its element and must run Drop at scope end. Two Loggers
// contributing 1 and 4 to a counter — if both drops fire, c == 5.
struct Logger { counter: *mut u32, value: u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe { *self.counter = *self.counter + self.value; }
    }
}

fn answer() -> u32 {
    let mut c: u32 = 0;
    {
        let (_a, _b) = (
            Logger { counter: &mut c as *mut u32, value: 1u32 },
            Logger { counter: &mut c as *mut u32, value: 4u32 },
        );
    }
    c
}
