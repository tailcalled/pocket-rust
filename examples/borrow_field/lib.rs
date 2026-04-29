struct Point { x: u32, y: u32 }

fn borrow_field(pt: &Point) -> &u32 {
    &pt.x
}

fn first_mut(p: &mut Point) -> &mut u32 {
    &mut p.x
}

fn answer() -> u32 {
    let mut pt = Point { x: 7, y: 14 };
    let _v: u32 = { let r: &u32 = borrow_field(&pt); *r };
    let m: &mut u32 = first_mut(&mut pt);
    *m = 42;
    pt.x
}
