// For-loop desugar synthesizes `let __iter = expr` and immediately
// takes `&mut __iter` to call `Iterator::next`. The synth binding
// must land in `Storage::MemoryAt` (dynamic shadow-stack slot) — if
// it accidentally got `Storage::Local` (wasm locals, no address), the
// `&mut __iter` borrow would yield the address of nothing and the
// iterator would never advance.
//
// Iterator state: each `next` mutates `n` in-place via `&mut self`.
// If __iter were stored as wasm locals, the mutation wouldn't survive
// across iterations and the loop would either run forever (next always
// sees n=0) or terminate immediately (next sees stale max).
//
// Three iterations: 10 + 14 + 18 = 42.

struct Stepper { n: u32, step: u32, max: u32 }

impl Iterator for Stepper {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        if self.n >= self.max {
            Option::None
        } else {
            let v: u32 = self.n;
            self.n = self.n + self.step;
            Option::Some(v)
        }
    }
}

fn answer() -> u32 {
    let s: Stepper = Stepper { n: 10, step: 4, max: 20 };
    let mut sum: u32 = 0;
    for x in s {
        sum = sum + x;
    }
    sum
}
