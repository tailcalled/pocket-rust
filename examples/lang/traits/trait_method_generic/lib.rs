trait Pick {
    fn pick<U>(self, a: U, b: U) -> U;
}

struct First {}
impl Pick for First {
    fn pick<V>(self, a: V, b: V) -> V { a }
}

fn use_pick<T: Pick>(t: T) -> u32 {
    t.pick::<u32>(11, 22)
}

fn answer() -> u32 {
    use_pick(First {})
}
