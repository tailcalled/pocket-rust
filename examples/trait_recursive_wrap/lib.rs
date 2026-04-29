trait Show { fn show(self) -> u32; }

struct Wrap<T> { inner: T }

impl Show for u32 {
    fn show(self) -> u32 { self }
}

impl<T: Show> Show for Wrap<T> {
    fn show(self) -> u32 { self.inner.show() }
}

fn answer() -> u32 {
    let w: Wrap<Wrap<u32>> = Wrap { inner: Wrap { inner: 42 } };
    w.show()
}
