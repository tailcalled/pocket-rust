struct Inner { v: u32 }
struct Outer { a: Inner, b: Inner }

fn read_v(i: &Inner) -> u32 { i.v }

fn answer() -> u32 {
    let o = Outer { a: Inner { v: 7 }, b: Inner { v: 99 } };
    let r: &Inner = &o.a;
    read_v(r)
}
