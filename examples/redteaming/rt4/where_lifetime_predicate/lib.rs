// `where 'a: 'b` — a lifetime-on-lifetime predicate. The LHS is a
// lifetime, not a type. `parse_where_clause_opt` calls `parse_type`
// for the LHS, which rejects the lifetime token with a confusing
// "expected type, got lifetime" error.
//
// Real Rust accepts this — lifetime predicates are how outlives
// obligations are spelled in where-clauses. Pocket-rust's lifetime
// checking is Phase B structural-only, so the predicate doesn't
// need to be enforced semantically yet, but the parser should at
// least carry it.

fn lift<'a, 'b>(x: &'a u32, _y: &'b u32) -> &'b u32
where
    'a: 'b,
{
    x
}

pub fn answer() -> u32 {
    let a: u32 = 21u32;
    let b: u32 = 21u32;
    let r = lift(&a, &b);
    *r + 21u32
}
