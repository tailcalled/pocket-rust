// Method body inside an impl uses a method declared on a trait
// the impl's where-clause requires. Without merging the where-clause
// into the impl's type-param bound table, `self.v.must_have()` fails
// because typeck sees `T`'s bound list as empty.
//
// With the fix, the where-clause's `T: Required` predicate gets
// merged into `impl_type_param_bounds[T]` at setup time, so method
// dispatch on `T` finds the Required trait via the bound.

trait Required {
    fn must_have(self) -> u32;
}

impl Required for u32 {
    fn must_have(self) -> u32 {
        self
    }
}

struct Holder<T> {
    v: T,
}

impl<T> Holder<T>
where
    T: Required,
{
    fn check(self) -> u32 {
        self.v.must_have()
    }
}

pub fn answer() -> u32 {
    Holder { v: 42u32 }.check()
}
