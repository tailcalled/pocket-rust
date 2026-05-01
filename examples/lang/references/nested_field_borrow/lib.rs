struct Inner<'a> { r: &'a u32 }
struct Outer<'a> { i: Inner<'a> }

fn answer() -> u32 {
    let x: u32 = 42;
    let o: Outer = Outer { i: Inner { r: &x } };
    let r: &u32 = o.i.r;
    *r
}
