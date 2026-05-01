fn answer() -> u32 {
    let c: std::dummy::Cell<u32> = std::dummy::Cell::<u32>::new(42);
    let r: &u32 = c.get_ref();
    *r
}
