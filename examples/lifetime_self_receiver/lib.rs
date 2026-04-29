struct Container<'a> { inner: &'a u32 }

impl<'a> Container<'a> {
    fn get(&'a self) -> &'a u32 {
        self.inner
    }
}

fn answer() -> u32 {
    let v: u32 = 42;
    let c: Container<'_> = Container { inner: &v };
    let r: &u32 = c.get();
    *r
}
