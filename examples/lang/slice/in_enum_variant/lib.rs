// `&[T]` as an enum variant payload. The enum's max-payload byte
// size must include the 8 bytes of the fat ref. Match it back out,
// observe `len`.
enum Choice<'a> {
    Empty,
    Slice(&'a [u32]),
}

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    let c: Choice = Choice::Slice(v.as_slice());
    match c {
        Choice::Slice(s) => (s.len() as u32) + 38,
        Choice::Empty => 0,
    }
}
