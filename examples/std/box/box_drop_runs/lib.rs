// Box's Drop runs its inner T's destructor at scope-end. Tracked
// counter through a raw pointer; `Tracked::drop` increments it.

struct Tracked { ctr: *mut u32 }

impl Drop for Tracked {
    fn drop(&mut self) {
        unsafe { *self.ctr = *self.ctr + 1; }
    }
}

fn answer() -> u32 {
    let counter_bytes: *mut u8 = unsafe { ¤alloc(4) };
    let counter: *mut u32 = unsafe { ¤cast::<u32, u8>(counter_bytes) };
    unsafe { *counter = 0; }
    {
        let _b: Box<Tracked> = Box::new(Tracked { ctr: counter });
        // _b drops here → its inner Tracked drops → ctr += 1.
    }
    unsafe { *counter * 42 }
}
