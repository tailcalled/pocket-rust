// Impl-block-level lifetime where-clauses are silently dropped.
//
// `src/typeck/setup.rs` processes `ib.where_clause` only for
// `WherePredicate::Type` — the `WherePredicate::Lifetime` arm falls
// through with the comment "lifetime predicates pass through (parsed
// but not yet enforced)". When borrowck's L1 reads
// `FnSymbol.lifetime_predicates`, it sees only the FUNCTION's own
// where-clause, never the impl's.
//
// Concretely: an impl declares `where 'a: 'b` so all methods in it
// can rely on the relation. A method body that uses the relation
// (e.g. returns `&'a u32` as `&'b u32`) is sound — but borrowck
// rejects because the predicate isn't visible in the method's
// `lifetime_predicates`.
//
// Real Rust accepts: impl-level where-clauses constrain every method
// in the impl.
//
// Expected post-fix: `setup.rs`'s `register_function` (or the
// per-impl-method registration path) merges
// `ib.where_clause`'s lifetime predicates into the method's own
// `lifetime_predicates` before constructing the `FnSymbol`. Mirrors
// what rt4#2 did for type predicates.

struct Holder<'a, 'b> {
    a: &'a u32,
    b: &'b u32,
}

impl<'a, 'b> Holder<'a, 'b>
where
    'a: 'b,
{
    fn shorten(self) -> &'b u32 {
        // Body returns `&'a u32` as `&'b u32`. Sound iff `'a: 'b`,
        // which the impl-level where-clause declares. Today's
        // borrowck rejects because it only sees the method's own
        // (empty) where-clause.
        self.a
    }
}

pub fn answer() -> u32 {
    let a: u32 = 21u32;
    let b: u32 = 21u32;
    let h = Holder { a: &a, b: &b };
    *h.shorten() + 21u32
}
