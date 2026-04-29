struct Pair<T, U> { first: T, second: U }

impl<T> Pair<u32, T> {
    fn first_field(self) -> u32 { self.first }
}

fn answer() -> u32 {
    let p: Pair<u32, u32> = Pair { first: 42, second: 7 };
    p.first_field()
}
