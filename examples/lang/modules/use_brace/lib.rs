use std::{Drop, dummy};

struct L { p: *mut u32 }

impl Drop for L {
    fn drop(&mut self) {
        unsafe { *self.p = 1; }
    }
}

fn answer() -> u32 {
    dummy::id(42) as u32
}
