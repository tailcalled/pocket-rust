// `impl` block with a where-clause that names a bound `T` does
// **not** satisfy. Real Rust rejects the impl declaration itself
// at the use site: `Holder::<NotRequired>::ok()` would error with
// "the trait bound `NotRequired: Required` is not satisfied" because
// the impl's where-clause excludes `NotRequired` from the impl's
// applicable set.
//
// Pocket-rust currently accepts: `ImplBlock.where_clause` is parsed
// but no setup pass reads it. The impl's bounds aren't merged into
// the impl-level type-param-bound table that controls whether a
// concrete type-arg is admissible at the impl. So `Holder::<NotRequired>`
// is constructed and `ok()` is called as if no where-clause existed.
//
// Expected post-fix: typeck rejects this with a where-clause-not-
// satisfied diagnostic naming `NotRequired: Required`.

trait Required {
    fn must_have(self) -> u32;
}

struct NotRequired;

struct Holder<T> {
    v: T,
}

impl<T> Holder<T> where T: Required {
    fn ok() -> u32 {
        42u32
    }
}

pub fn answer() -> u32 {
    Holder::<NotRequired>::ok()
}
