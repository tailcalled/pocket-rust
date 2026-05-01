struct Pair<T, U> { first: T, second: U }

impl<T> Pair<T, T> {
    fn first_field(self) -> T { self.first }
}

fn answer() -> u32 {
    let p: Pair<u32, u32> = Pair { first: 42, second: 7 };
    p.first_field()
}
