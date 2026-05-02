fn answer() -> u32 {
    let b: Box<u32> = Box::new(42);
    *b
}
