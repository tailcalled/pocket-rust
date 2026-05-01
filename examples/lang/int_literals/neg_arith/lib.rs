// Unary minus on a non-literal expression desugars to `<T as VecSpace>::neg`.
fn answer() -> u32 {
    let x: i32 = 50;
    let y: i32 = -x;
    let z: i32 = 92;
    (y + z) as u32
}
