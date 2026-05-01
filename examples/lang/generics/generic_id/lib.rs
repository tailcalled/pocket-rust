fn id<T>(x: T) -> T {
    x
}

fn read_through<T>(r: &T) -> &T {
    r
}

fn answer() -> u32 {
    let _a: u32 = id(42);
    let _b: u8 = id::<u8>(7);
    let pt: u32 = 100;
    let r: &u32 = read_through(&pt);
    *r
}
