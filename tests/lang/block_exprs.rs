// `{ stmts; tail }` block expressions.

use super::*;

#[test]
fn block_expr_returns_11() {
    expect_answer("lang/block_exprs/block_expr", 11i32);
}
