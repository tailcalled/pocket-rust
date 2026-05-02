// `*b = value` writes through `<Box<T> as DerefMut>::deref_mut`.
fn answer() -> u32 {
    let mut b: Box<u32> = Box::new(0);
    *b = 42;
    *b
}
