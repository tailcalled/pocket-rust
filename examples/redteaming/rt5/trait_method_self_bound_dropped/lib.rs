// `trait Foo { fn x() -> u32 where Self: Bar; }` — the trait method
// declares a where-clause whose LHS is `Self`. rt4#4 added merging
// for `where T: Bound` (Param-LHS); but `Self` doesn't appear in
// `type_params` (Self is implicit via self_target), so the merge
// loop's `RType::Param(name)` match fails. The predicate is silently
// dropped: impls of Foo can satisfy `x()` without implementing Bar.
//
// Expected post-fix: at impl-validation time, predicates with `Self`
// LHS are checked against the impl's target type. An impl of `Foo`
// for a type that doesn't impl `Bar` is rejected.

trait Bar {
    fn must_have(&self) -> u32;
}

trait Foo {
    fn x() -> u32
    where
        Self: Bar;
}

struct NoBar;

// Real Rust rejects this: NoBar doesn't impl Bar, so it can't impl
// Foo (which requires Self: Bar on `x`).
impl Foo for NoBar {
    fn x() -> u32 {
        0u32
    }
}

pub fn answer() -> u32 {
    NoBar::x()
}
