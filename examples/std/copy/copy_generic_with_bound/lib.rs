struct Wrap<T> { inner: T }

impl<T: Copy> Copy for Wrap<T> {}

fn answer() -> u32 {
    let w: Wrap<u32> = Wrap { inner: 42 };
    let v: Wrap<u32> = w;
    w.inner
}
