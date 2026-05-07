// `where` clauses — predicate list after the fn signature.
// Param-LHS predicates are merged into the matching type-param's
// inline bounds at setup; complex-LHS predicates are stored on the
// GenericTemplate and enforced after substitution at call sites.

use super::*;

// Simple Param-LHS predicate, equivalent to inline `<T: Show>` bound.
// Verifies the where parser, the merge into type_param_bounds, and
// that downstream symbolic dispatch finds the trait method.
#[test]
fn where_clause_param_lhs_compiles() {
    let bytes = compile_inline(
        "trait Doubler { fn double(self) -> u32; }\n\
         impl Doubler for u32 { fn double(self) -> u32 { self + self } }\n\
         fn twice<T>(x: T) -> u32 where T: Doubler { x.double() }\n\
         pub fn answer() -> u32 { twice(21u32) }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

// Multi-bound where-clause: `T: A + B` parses and merges both bounds.
#[test]
fn where_clause_multi_bound_compiles() {
    let bytes = compile_inline(
        "trait Tagged { fn tag(self) -> u32; }\n\
         trait Named { fn label(self) -> u32; }\n\
         impl Tagged for u32 { fn tag(self) -> u32 { self } }\n\
         impl Named for u32 { fn label(self) -> u32 { 1u32 } }\n\
         fn combine<T>(x: T) -> u32 where T: Tagged + Named, T: Copy {\n\
             x.tag() + x.label()\n\
         }\n\
         pub fn answer() -> u32 { combine(41u32) }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

// Where-clause on an APIT-generated anonymous type-param works the
// same as on a user-written one.
#[test]
fn where_clause_with_apit_compiles() {
    let bytes = compile_inline(
        "fn apply(f: impl Fn(u32) -> u32) -> u32 where u32: Copy { f.call((10u32,)) }\n\
         pub fn answer() -> u32 { apply(|x| x + 5u32) }",
    );
    assert_eq!(answer_u32(&bytes), 15);
}

// Negative: a where-clause predicate with a fully-concrete LHS that
// has no matching impl is rejected at setup time.
#[test]
fn where_clause_unsatisfied_concrete_lhs_is_rejected() {
    let err = compile_source(
        "trait MissingImpl { fn x(self) -> u32; }\n\
         fn f() where u32: MissingImpl { }\n\
         fn answer() -> u32 { f(); 0u32 }",
    );
    assert!(
        err.contains("where-clause predicate not satisfied"),
        "expected unsatisfied-where-clause error, got: {}",
        err,
    );
}

// Negative: a complex-LHS predicate that's unsatisfied at the call
// site (after type-param substitution) errors with the call-site
// diagnostic, not the setup-time one.
#[test]
fn where_clause_unsatisfied_complex_lhs_at_call_site_is_rejected() {
    // `(T,)` is a tuple-of-T. We require it to implement `MissingImpl`
    // but no impl exists → the substituted predicate `(u32,):
    // MissingImpl` fails at the call site.
    let err = compile_source(
        "trait MissingImpl { fn x(self) -> u32; }\n\
         fn f<T>(_t: T) where (T,): MissingImpl { }\n\
         fn answer() -> u32 { f(0u32); 0u32 }",
    );
    assert!(
        err.contains("where-clause predicate not satisfied at call site"),
        "expected call-site unsatisfied-where error, got: {}",
        err,
    );
}

// `where 'a: 'b` — outlives predicate. Stored on the FnSymbol's
// `lifetime_predicates` (Phase B carry-only); both LHS and bounds
// must be in the enclosing fn/impl's lifetime scope.
#[test]
fn where_lifetime_predicate_compiles() {
    let bytes = compile_inline(
        "fn pick<'a, 'b>(x: &'a u32, _y: &'b u32) -> &'b u32 where 'a: 'b { x }\n\
         pub fn answer() -> u32 { let a: u32 = 21u32; let b: u32 = 21u32; *pick(&a, &b) + 21u32 }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}

// Negative: a `where` predicate naming an undeclared lifetime is
// rejected with an "undeclared lifetime" diagnostic.
#[test]
fn where_lifetime_undeclared_is_rejected() {
    let err = compile_source(
        "fn pick<'a>(x: &'a u32) -> &'a u32 where 'a: 'b { x }\n\
         fn answer() -> u32 { 0u32 }",
    );
    assert!(
        err.contains("undeclared lifetime"),
        "expected undeclared-lifetime error, got: {}",
        err,
    );
}

// `T: Trait + 'a` — trailing lifetime bound on a type-where
// predicate. The lifetime is validated for in-scope-ness; the trait
// bound is processed normally.
#[test]
fn where_type_with_trailing_lifetime_compiles() {
    let bytes = compile_inline(
        "trait Show { fn show(self) -> u32; }\n\
         impl Show for u32 { fn show(self) -> u32 { self } }\n\
         fn run<'a, T>(t: T, _r: &'a u32) -> u32 where T: Show + 'a { t.show() }\n\
         pub fn answer() -> u32 { let r: u32 = 0u32; run(42u32, &r) }",
    );
    assert_eq!(answer_u32(&bytes), 42);
}
