struct Inner { v: u32 }
struct Outer { inner: Inner }

impl Outer {
    fn inner_ref(&self) -> &Inner {
        &self.inner
    }
}

fn answer() -> u32 {
    let o = Outer { inner: Inner { v: 42 } };
    let r: &Inner = o.inner_ref();
    r.v
}
