// rt4#2's fix walks `ImplBlock.where_clause` and merges Param-LHS
// preds into `impl_type_param_bounds`. But complex-LHS preds
// (anything that doesn't resolve to `RType::Param(name)` of an
// impl-level type-param) fall through silently — they're parsed
// but never stored or enforced.
//
// Concrete-LHS preds in particular should be checked at setup time
// (the LHS is fully known, so any predicate violation is a static
// fact). Pocket-rust drops the check, accepting an impl whose
// where-clause is unsatisfiable.
//
// Expected post-fix: setup walks the impl's complex-LHS predicates,
// resolves each predicate's LHS, and calls `solve_impl` against
// the (concrete) trait — failure errors at the impl declaration.

trait MissingTrait {
    fn never_called(self) -> u32;
}

struct Holder<T> {
    v: T,
}

// `(u32,): MissingTrait` is fully concrete and definitely false —
// no impl of MissingTrait for the `(u32,)` tuple exists in scope.
// Real Rust rejects this impl declaration outright.
impl Holder<u32>
where
    (u32,): MissingTrait,
{
    fn ok() -> u32 {
        42u32
    }
}

pub fn answer() -> u32 {
    Holder::<u32>::ok()
}
