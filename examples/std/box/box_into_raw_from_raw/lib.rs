// `Box::into_raw(b)` hands off ownership; `Box::from_raw(p)` takes
// it back. Round-tripping through raw shouldn't double-free or run
// T's destructor extra times — the second box's drop frees the
// buffer cleanly.

fn answer() -> u32 {
    let b1: Box<u32> = Box::new(42);
    let p: *mut u32 = Box::into_raw(b1);
    // Caller now owns the buffer at `p`. Wrap it back into a Box
    // and read the value through deref.
    let b2: Box<u32> = unsafe { Box::from_raw(p) };
    *b2
}
