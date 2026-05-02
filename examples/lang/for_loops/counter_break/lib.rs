// `break` inside for-loop body exits the loop. Counts up to (but
// not including) the break target.

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
    let c: Counter = Counter { n: 0, max: 100 };
    for x in c {
        if x == 10 { break; }
        sum = sum + x;
    }
    // Sum of 0..9 = 45. Subtract 3 to land at 42.
    sum - 3
}
