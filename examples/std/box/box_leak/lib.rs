// `Box::leak(b)` consumes the Box and returns a `&'static mut T`
// that lives forever. Box's destructor is suppressed (the buffer
// is never freed).

fn answer() -> u32 {
    let b: Box<u32> = Box::new(0);
    let r: &mut u32 = Box::leak(b);
    *r = 42;
    *r
}
