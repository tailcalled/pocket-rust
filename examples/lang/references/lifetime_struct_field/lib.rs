struct Inner { x: u32 }

struct Wrapper<'a> { inner: &'a Inner }

fn answer() -> u32 {
    let i: Inner = Inner { x: 42 };
    let w: Wrapper<'_> = Wrapper { inner: &i };
    let r: &Inner = w.inner;
    r.x
}
