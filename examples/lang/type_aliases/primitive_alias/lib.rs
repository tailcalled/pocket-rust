// Simplest case: alias for a primitive. The alias is fully
// transparent — `MyInt` and `u32` interchange in arithmetic, casts,
// and comparisons.

pub type MyInt = u32;

fn answer() -> u32 {
    let x: MyInt = 12u32;
    let y: u32 = 30u32;
    x + y
}
