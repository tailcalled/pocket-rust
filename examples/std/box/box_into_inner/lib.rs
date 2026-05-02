// `Box::into_inner(b)` consumes the Box, returning the heap-resident
// T (and freeing the buffer without running T's destructor).
fn answer() -> u32 {
    let b: Box<u32> = Box::new(42);
    let v: u32 = Box::into_inner(b);
    v
}
