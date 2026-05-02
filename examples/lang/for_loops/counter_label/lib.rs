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
    'outer: for x in (Counter { n: 0, max: 100 }) {
        for y in (Counter { n: 0, max: 100 }) {
            if x == 6 && y == 7 {
                sum = x * y;
                break 'outer;
            }
        }
    }
    sum
}
