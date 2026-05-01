struct Point {
    x: u32,
    y: u32,
}

fn answer() -> u32 {
    unsafe {
        let p: *mut u8 = ¤alloc(8);
        let pp: *mut Point = ¤cast::<Point, u8>(p);
        *pp = Point { x: 7, y: 35 };
        let v: u32 = (*pp).x + (*pp).y;
        ¤free(p);
        v
    }
}
