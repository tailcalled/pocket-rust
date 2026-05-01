// Pass `&[u32]` through a generic function. T = &[u32]. The generic
// body sees `T` (a Param), but at mono time it lands on the fat-ref
// shape — flatten_rtype, byte_size_of, ABI must all work.
fn id<T>(x: T) -> T { x }

fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(11);
    v.push(12);
    v.push(13);
    v.push(14);
    let s = id::<&[u32]>(v.as_slice());
    (s.len() as u32) + 38
}
