// Generic function with the most natural Rust signature for "needs
// `+`": `fn double<T: Add<T, Output = T>>(x: T) -> T { x + x }`. At
// the call site `double::<u32>(21)`, the bound's assoc-constraint
// `Output = T` should be substituted under T=u32 before being
// compared against the impl's actual binding (`Output = u32`). The
// current implementation skips that substitution and reports
// "type mismatch on associated type `Add::Output`: expected `T`,
// got `u32` (from `impl Add for u32`)" — telling the user that u32
// fails a constraint that, after substitution, u32 trivially
// satisfies.
//
// Expected: 42.

fn double<T: Add<T, Output = T> + Copy>(x: T) -> T {
    x + x
}

fn answer() -> u32 {
    double::<u32>(21)
}
