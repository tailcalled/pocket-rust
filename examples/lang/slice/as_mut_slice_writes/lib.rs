// Writing through `Vec::as_mut_slice` modifies the underlying buffer.
// `*s.get_mut(0).unwrap() = 100` style flow.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(0);
    v.push(0);
    v.push(0);
    {
        let s: &mut [u32] = v.as_mut_slice();
        match s.get_mut(0) {
            Option::Some(r) => { *r = 14; }
            Option::None => {}
        }
        match s.get_mut(1) {
            Option::Some(r) => { *r = 14; }
            Option::None => {}
        }
        match s.get_mut(2) {
            Option::Some(r) => { *r = 14; }
            Option::None => {}
        }
    }
    let s: &[u32] = v.as_slice();
    let a: u32 = match s.get(0) { Option::Some(r) => *r, Option::None => 0 };
    let b: u32 = match s.get(1) { Option::Some(r) => *r, Option::None => 0 };
    let c: u32 = match s.get(2) { Option::Some(r) => *r, Option::None => 0 };
    a + b + c  // 14 + 14 + 14 = 42
}
