// `for _ in iter` — wildcard pattern, body doesn't bind the
// yielded value. Used for "do this N times" idioms.

struct Counter { n: u32, max: u32 }

impl Iterator for Counter {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        if self.n < self.max {
            let v: u32 = self.n;
            self.n = self.n + 1;
            Option::Some(v)
        } else {
            Option::None
        }
    }
}

fn answer() -> u32 {
    let mut count: u32 = 0;
    let c: Counter = Counter { n: 0, max: 7 };
    for _ in c {
        count = count + 6;
    }
    count
}
