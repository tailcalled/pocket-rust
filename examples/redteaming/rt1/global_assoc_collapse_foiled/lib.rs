// Demonstrates that `assoc_always_equals_self` is a *global* property
// over (trait, assoc-name): once the user adds *any* `impl Add for T`
// where `Output != T`, the collapse rule stops firing for *all* types,
// including the primitive integer impls. The visible symptom is that
// `30 + 12` (a chain of literal arithmetic) no longer typechecks.
//
// Expected: 42 (literal arithmetic should remain unaffected by an
// unrelated user impl).

struct Wrap { v: u32 }

impl Add for Wrap {
    type Output = u32;
    fn add(self, other: Wrap) -> u32 { self.v + other.v }
}

fn answer() -> u32 {
    30 + 12
}
