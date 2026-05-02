// With only the blanket `impl<T> Trait for T` (no str impl), the
// blanket method recv type `&T` would need T=str at chain level
// `&str` — rejected by Sized. The autoref level `&&str` then binds
// T=&str (Sized passes) → match. Validates that Sized exclusion at
// one level still permits a valid alternative further down the chain.

trait Trait {
    fn m(&self) -> u32;
}

impl<T> Trait for T {
    fn m(&self) -> u32 { 7 }
}

fn answer() -> u32 {
    let s: &str = "x";
    s.m()
}
