// Two impls that don't actually overlap (despite appearances): the
// blanket `impl<T> MyTrait for T` carries an implicit `T: Sized`
// bound, and `str` is unsized — so the blanket doesn't cover `str`.
//
// For `s: &str; s.test()`:
// - At candidate level `&str` (recv as-is):
//     - str impl method recv type = `&str` → matches directly.
//     - blanket method recv type = `&T`. Would need T=str, which
//       fails Sized → blanket excluded.
//   → str impl wins, returns 0. Mirrors rustc.

trait MyTrait {
    fn test(&self) -> usize;
}

impl MyTrait for str {
    fn test(&self) -> usize { 0 }
}

impl<T> MyTrait for T {
    fn test(&self) -> usize { 1 }
}

fn answer() -> usize {
    let s: &str = "hello";
    s.test()
}
