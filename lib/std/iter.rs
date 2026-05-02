use crate::option::Option;

// External iteration via `next() -> Option<Item>`. `for x in iter`
// calls `Iterator::next(&mut iter)` repeatedly until `None`.
//
// In Rust, the for-loop also runs `IntoIterator::into_iter` on the
// loop's iter-expression first to convert (e.g.) `Vec<T>` into a
// `vec::IntoIter<T>`. Pocket-rust doesn't yet have `IntoIterator`,
// so the loop's iter-expression must already implement `Iterator`
// directly — write `vec.into_iter()` (or `vec.iter()` for refs)
// explicitly.
pub trait Iterator {
    type Item;
    fn next(&mut self) -> Option<Self::Item>;
}

// TODO: trait IntoIterator { type Item; type IntoIter: Iterator<Item = Self::Item>; fn into_iter(self) -> Self::IntoIter; }
//   — needs the for-loop to call into_iter on its iter-expression first.
// TODO: blanket `impl<T: Iterator> IntoIterator for T { type Item = T::Item; type IntoIter = T; fn into_iter(self) -> T { self } }`
//   — needs blanket impls + IntoIterator above.
// TODO: standard adapters (`map`, `filter`, `chain`, `zip`, …) — each
//   needs its own struct + Iterator impl, plus closures (which pocket-
//   rust doesn't have). Land Iterator basics first.
// TODO: `count` / `sum` / `product` / `collect` / `fold` — terminal
//   operations; need closures (fold/collect) or numeric default-Add/Mul
//   (sum/product).
