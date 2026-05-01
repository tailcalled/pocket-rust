// Verifies that `Vec<T>::drop` runs `T`'s destructor on every
// initialized element. We push three `Tracked` values into a Vec
// inside an inner block, then read a heap-allocated counter to see
// how many times `Tracked`'s `drop` fired. Expected: 3.
struct Tracked {
    ctr: *mut u32,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        unsafe {
            let p: *mut u32 = self.ctr;
            *p = *p + 1;
        }
    }
}

fn answer() -> u32 {
    let counter_bytes: *mut u8 = unsafe { ¤alloc(4) };
    let counter: *mut u32 = unsafe { ¤cast::<u32, u8>(counter_bytes) };
    unsafe { *counter = 0; }
    {
        let mut v: Vec<Tracked> = Vec::new();
        v.push(Tracked { ctr: counter });
        v.push(Tracked { ctr: counter });
        v.push(Tracked { ctr: counter });
    }
    unsafe { *counter }
}
