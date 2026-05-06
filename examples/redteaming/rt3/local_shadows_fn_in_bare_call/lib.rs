// Bare-call sugar in `check_call` only intercepts when the local
// resolves to a synthesized closure struct. If a local of any other
// type happens to share its name with a function in scope, the
// fall-through path proceeds with normal function lookup and CALLS
// THE FUNCTION — Rust would reject `name(args)` here with "expected
// function, found u32" because the local shadows the function.
//
// Architectural shape: pocket-rust's resolution order in `check_call`
// is "function-table first, then locals". The bare-closure-call patch
// added a closure-typed-local check at the top, but didn't generalize
// to "any local shadows the function". Resolution should always
// prefer the local over the fn entry; the fn lookup should only fire
// when no local with that name exists.
//
// At runtime: `doubled(5)` here resolves to the function, returns 10.
// Real Rust would not even compile (E0618 "expected function").
//
// Expected post-fix: the call is REJECTED at typeck — local `doubled`
// of type `u32` is not callable.

fn doubled(x: u32) -> u32 {
    x * 2u32
}

pub fn answer() -> u32 {
    let doubled: u32 = 100u32;
    doubled(5u32)
}
