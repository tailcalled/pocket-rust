struct Pair<T, U> { first: T, second: U }

impl<T, U> Pair<T, U> {
    fn new(first: T, second: U) -> Self {
        Self { first: first, second: second }
    }

    fn first_ref(&self) -> &T {
        &self.first
    }

    fn echo<W>(&self, w: W) -> W {
        w
    }
}

fn answer() -> u32 {
    let p: Pair<u32, u32> = Pair::<u32, u32>::new(7, 35);
    let r: &u32 = p.first_ref();
    let _e: u32 = p.echo::<u32>(99);
    *r
}
