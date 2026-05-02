// Range expressions desugar at parse-time to `std::ops::Range`
// struct literals, so `start` / `end` are accessible as fields on
// the resulting value. `(2u32..5u32).end - (2u32..5u32).start == 3`.
fn answer() -> u32 {
    let r: Range<u32> = 2u32..5u32;
    r.end - r.start
}
