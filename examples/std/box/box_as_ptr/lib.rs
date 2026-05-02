// `Box::as_ptr(&b)` borrows the buffer as `*const T`; `as_mut_ptr`
// gives `*mut T`. Box still owns the buffer; the returned ptr is
// good for as long as the Box is.

fn answer() -> u32 {
    let mut b: Box<u32> = Box::new(40);
    let pm: *mut u32 = Box::as_mut_ptr(&mut b);
    unsafe { *pm = 42; }
    let pc: *const u32 = Box::as_ptr(&b);
    unsafe { *pc }
}
