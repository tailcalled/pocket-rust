// `where 'a: 'static` — a lifetime predicate naming the special
// `'static` lifetime. rt4#6's validation iterates the enclosing
// fn/impl's `lifetime_param_names` to verify each named lifetime
// in the predicate is declared. `'static` isn't a user-declared
// lifetime parameter — it's a built-in — so the validation
// rejects it as "undeclared lifetime `'static`".
//
// Real Rust accepts: `'static` is always in scope, and the
// predicate `'a: 'static` constrains `'a` to outlive the program.
//
// Expected post-fix: validation treats `'static` (and any other
// built-in lifetime) as implicitly in scope.

fn must_be_static<'a>(x: &'a u32) -> u32
where
    'a: 'static,
{
    *x
}

pub fn answer() -> u32 {
    let x: u32 = 42u32;
    must_be_static(&x)
}
