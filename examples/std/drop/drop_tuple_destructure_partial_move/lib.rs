// Tuple destructure with partial move: take(a) consumes the
// first binding (its Drop fires inside take's frame, +1); _b
// stays in scope and drops at the inner block end (+4). Final
// counter is 5.
struct Logger { counter: *mut u32, value: u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe { *self.counter = *self.counter + self.value; }
    }
}

fn take(_l: Logger) {}

fn answer() -> u32 {
    let mut c: u32 = 0;
    {
        let (a, _b) = (
            Logger { counter: &mut c as *mut u32, value: 1u32 },
            Logger { counter: &mut c as *mut u32, value: 4u32 },
        );
        take(a);
    }
    c
}
