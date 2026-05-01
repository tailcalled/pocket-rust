// `std::cmp::PartialEq` / `Eq` / `PartialOrd` / `Ord`: comparison
// operator desugar (`== != < <= > >=`) and supertrait dispatch.

use super::*;

// `==` desugars to `<T as Eq>::eq(&self, &other)` returning bool. `5
// == 5` is true → returns 11.
#[test]
fn op_eq_in_if_returns_11() {
    expect_answer("std/cmp/op_eq_in_if", 11i32);
}

// `<` desugars to `<T as Ord>::lt(&self, &other)`. Signed lt picks
// `¤i32_lt` (signed wasm op) — `5 < 7` is true → returns 11.
#[test]
fn op_ord_in_if_returns_11() {
    expect_answer("std/cmp/op_ord_in_if", 11i32);
}

// `<T: Eq>` calling `t.eq(&u)`: `eq` is declared on the supertrait
// PartialEq. Method dispatch through bounds walks the supertrait
// closure to find it.
#[test]
fn supertrait_eq_via_partialeq_returns_42() {
    expect_answer("std/cmp/supertrait_eq_via_partialeq", 42u32);
}

#[test]
fn partialord_lt_returns_42() {
    // `<T: PartialOrd>` calling `t.lt(&u)`: PartialOrd's own method.
    expect_answer("std/cmp/partialord_lt", 42u32);
}
