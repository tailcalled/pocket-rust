// Match ergonomics through `&mut T`: the auto-peel inherits RefMut so
// `Some(x)` against `&mut Option<u32>` binds x as `&mut u32`. Mutating
// through it updates the original.

fn double_in_place(o: &mut Option<u32>) {
    match o {
        Option::Some(x) => { *x = *x * 2u32; }
        Option::None => {}
    }
}

fn answer() -> u32 {
    let mut o: Option<u32> = Option::Some(21u32);
    double_in_place(&mut o);
    match o {
        Option::Some(v) => v,
        Option::None => 0u32,
    }
}
