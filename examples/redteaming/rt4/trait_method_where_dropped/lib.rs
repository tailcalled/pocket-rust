// Trait declares a method with a `where` clause: `fn x<T>() -> u32
// where T: Required`. The clause requires every caller to supply a
// `T` that implements `Required`. Real Rust enforces this at the
// call site: `Maker::<NotRequired>::x::<NotRequired>(...)` (or
// equivalent) would error.
//
// Pocket-rust drops the where-clause silently at trait setup:
// `resolve_trait_methods` doesn't walk `TraitMethodSig.where_clause`,
// so the method's `type_param_bounds` lists no constraint on `T`.
// The `impl` for `NotRequired` then satisfies the (empty) trait
// method bound, and the call goes through.
//
// Expected post-fix: pocket-rust rejects this at the call site with
// a diagnostic naming the unsatisfied `T: Required` bound.

trait Required {
    fn must_have(self) -> u32;
}

struct NotRequired;

trait Maker {
    fn x<T>() -> u32 where T: Required;
}

impl Maker for NotRequired {
    fn x<T>() -> u32 {
        0u32
    }
}

pub fn answer() -> u32 {
    NotRequired::x::<NotRequired>()
}
