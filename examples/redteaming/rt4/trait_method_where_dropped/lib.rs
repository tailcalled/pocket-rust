// Trait method declares a where-clause on its type-param. The impl
// method's body uses `t.must_have()` — which compiles only when
// `T` carries the `Required` bound. Without the fix, the trait's
// where-clause was silently dropped, the impl method's `T` had no
// bound, and the body failed to dispatch `must_have`.
//
// With the fix:
//   1. `resolve_trait_methods` walks `m.where_clause`, merges
//      Param-LHS predicates into `TraitMethodEntry.type_param_bounds`.
//   2. `register_function` for the impl method (when the impl is
//      for a trait) looks up the trait method's bounds and merges
//      them onto the impl method's matching type-param slots.
//   3. The impl method body type-checks `t.must_have()` via the
//      inherited `T: Required` bound.

trait Required {
    fn must_have(self) -> u32;
}

impl Required for u32 {
    fn must_have(self) -> u32 {
        self
    }
}

trait Maker {
    fn x<T>(t: T) -> u32
    where
        T: Required;
}

struct UnitMaker;

impl Maker for UnitMaker {
    fn x<T>(t: T) -> u32 {
        t.must_have()
    }
}

pub fn answer() -> u32 {
    UnitMaker::x(42u32)
}
