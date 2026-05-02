// Drop ordering for destructured bindings: reverse declaration
// order. `let (_a, _b) = …` should drop _b first, then _a. The
// Logger's drop folds its `value` into a base-10 sequence, so the
// final counter encodes the visit order.
//
// Drop visiting `_b` first, then `_a`:  ((0*10) + 2) * 10 + 1 == 21
// (Wrong order — _a then _b — would give: ((0*10) + 1) * 10 + 2 == 12)
struct Logger { sink: *mut u32, value: u32 }

impl Drop for Logger {
    fn drop(&mut self) {
        unsafe {
            *self.sink = (*self.sink) * 10u32 + self.value;
        }
    }
}

fn answer() -> u32 {
    let mut s: u32 = 0;
    {
        let (_a, _b) = (
            Logger { sink: &mut s as *mut u32, value: 1u32 },
            Logger { sink: &mut s as *mut u32, value: 2u32 },
        );
    }
    s
}
