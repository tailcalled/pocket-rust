// Basic for-in: iterate a custom Counter type that yields 0..N.
// Each iteration calls `<Counter as Iterator>::next(&mut counter)`,
// matches the `Option<u32>` result; `Some(x)` binds and runs body,
// `None` exits. Sum = 1+2+3+4+5+6+7+8 = 36, +6 = 42.

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
    let mut sum: u32 = 0;
    let c: Counter = Counter { n: 1, max: 9 };
    for x in c {
        sum = sum + x;
    }
    sum + 6
}
