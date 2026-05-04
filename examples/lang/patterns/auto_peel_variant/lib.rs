// Match ergonomics: a non-reference variant pattern matched against
// a `&T` scrutinee auto-peels the reference. Inside the pattern's
// payload bindings, the default mode flips to Ref so `x` binds as
// `&u32` (not by-value u32). Equivalent to writing the explicit
// `&Option::Some(ref x) => *x` form.

fn pick(o: &Option<u32>) -> u32 {
    match o {
        Option::Some(x) => *x,
        Option::None => 0u32,
    }
}

fn answer() -> u32 {
    let o: Option<u32> = Option::Some(42u32);
    pick(&o)
}
