// match expression evaluating to `&[u32]`. Same multi-value story as
// `if`: each arm pushes a 2-i32 fat ref; the outer block must carry
// both halves out.
enum Side { Left, Right }

fn pick<'a>(side: Side, a: &'a [u32], b: &'a [u32]) -> &'a [u32] {
    match side {
        Side::Left => a,
        Side::Right => b,
    }
}

fn answer() -> u32 {
    let mut v1: Vec<u32> = Vec::new();
    v1.push(1);
    v1.push(2);
    v1.push(3);
    v1.push(4);
    let mut v2: Vec<u32> = Vec::new();
    v2.push(10);
    let s = pick(Side::Left, v1.as_slice(), v2.as_slice());
    (s.len() as u32) + 38
}
