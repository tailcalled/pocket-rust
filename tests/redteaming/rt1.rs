// Round 1 of red-team findings. Each test below pins one
// architectural problem in the compiler's current state. **Every
// test in this file is expected to fail** — the failure *is* the
// surfaced bug. When a problem is fixed, the corresponding test
// starts passing; that's the signal to retire the example or fold it
// into the regular `tests/lang/` suite.

use super::*;

// PROBLEM 1: `assoc_always_equals_self` is global per (trait,
// assoc_name). The collapse rule `<? as Add>::Output → ?` fires only
// when *every* registered Add impl binds Output equal to the impl's
// target type. As soon as any single user impl breaks the invariant
// (e.g. `impl Add for Wrap { type Output = u32; }`), the collapse
// stops firing for *all* types, including the primitive int impls.
// The user-visible symptom is that ordinary `30 + 12` literal
// arithmetic stops typechecking even though it has nothing to do
// with the user's Wrap type.
//
// Why it's architectural: the collapse is the load-bearing mechanism
// for chained literal arithmetic (`1 + 2 + 3`, `-(30+12)`, etc.) and
// for unifying arithmetic-result types with concrete return types
// (`fn answer() -> u32 { 30 + 12 }`). Tying it to a global "every
// impl agrees" check makes the inference behavior of literal code
// depend on whatever unrelated impls happen to exist in the crate
// graph — a textbook spooky-action-at-a-distance bug.
//
// Fix shape: per-impl resolution. When base is unknown but the trait
// has impls all sharing some property *for a candidate set of base
// types*, infer it locally; otherwise keep the projection unresolved
// and let downstream unification (with concrete types) drive
// resolution. The `assoc_always_equals_self` global check should not
// be used in inference.
#[test]
fn problem_1_global_assoc_collapse_foiled() {
    expect_answer("redteaming/rt1/global_assoc_collapse_foiled", 42i32);
}

// PROBLEM 2: `find_assoc_binding` walks every impl row whose
// `trait_path` matches and returns each impl's binding for the assoc
// name *without filtering by trait_args*. So with two
// `impl Mix<X> for Foo` rows declaring different `Output` bindings,
// querying `<Foo as Mix>::Output` returns 2 candidates and
// `concretize_assoc_proj` gives up (since `candidates.len() != 1`).
// This breaks impl-signature validation: registering the second
// impl of a generic trait that has assoc types fails at the trait-
// vs-impl-signature comparison with a misleading "wrong return type:
// trait declares `<Foo as ?>::Output`, impl has `i64`" — caused by
// the projection failing to concretize during validation.
//
// Why it's architectural: trait-args are a first-class part of impl
// identity (call sites differentiate `impl Mix<u32> for Foo` from
// `impl Mix<i64> for Foo` by trait_args). Resolving an assoc-type
// projection without consulting trait_args means the assoc-type
// system can't coexist with generic-trait impls in the simplest
// case — registering them fails before any call site is checked.
//
// Fix shape: thread trait_args through `concretize_assoc_proj` and
// its callers (notably `validate_trait_impl_signatures`) into
// `find_assoc_binding_with_args`, the variant that already exists
// but isn't being called from the impl-validation path.
#[test]
fn problem_2_find_assoc_binding_ignores_trait_args() {
    expect_answer(
        "redteaming/rt1/find_assoc_binding_ignores_trait_args",
        42i32,
    );
}

// PROBLEM 3: `is_mutable_place` (the gate that decides whether the
// `&mut Self` autoref level is tried during method dispatch) only
// walks Var/FieldAccess/TupleIndex chains via
// `extract_place_for_assign`. It returns false for `Deref`, even
// though `*p` for `p: &mut T` is a fully mutable place — plain
// assignment `*p = …;` works. So `*p += 1;` (which the parser
// desugars to `(*p).add_assign(1)`) fails dispatch and surfaces
// "no method `add_assign` on `u32`".
//
// Why it's architectural: the assignment statement (`*p = …;`) and
// the compound assignment statement (`*p += …;`) describe the same
// in-place mutation. They share the same place-mutability
// requirement. Having the two paths use different "is this a mutable
// place" predicates is a divergence that will recur every time a
// new mutating syntactic form is added.
//
// Fix shape: extend `extract_place_for_assign` (or a sibling
// predicate it shares with assignment) to recognize Deref of a
// `&mut T` or `*mut T` binding and Index where the receiver supports
// IndexMut, returning the same "yes, mutable place" verdict that
// `*p = …;` and `vec[i] = …;` already implicitly rely on.
#[test]
fn problem_3_is_mutable_place_misses_deref() {
    expect_answer("redteaming/rt1/is_mutable_place_misses_deref", 42i32);
}

// PROBLEM 4: same root cause as problem 3, exercised through the
// indexing path. `vec[i] += 1;` desugars to
// `(vec[i]).add_assign(1)`. The recv is an Index expression.
// `is_mutable_place` returns false for it, so the autoref-mut
// dispatch level is never tried, and the call fails with "no method
// `add_assign` on `u32`".
//
// Worth keeping as a separate test from problem 3 because the fix
// for the Deref case (special-casing `&mut T` derefs) doesn't
// automatically cover Index — Index/IndexMut is a trait-resolution
// path, and "mutable place" through an indexed access has to consult
// IndexMut's existence and the receiver's mutability. This is
// strictly more involved than the Deref fix and is worth its own
// follow-up.
#[test]
fn problem_4_is_mutable_place_misses_index() {
    expect_answer("redteaming/rt1/is_mutable_place_misses_index", 42i32);
}

// PROBLEM 5: layered consequence of problem 3 — `*p += 1;` for
// `p: *mut u32` (raw pointer), even inside an `unsafe { … }` block,
// fails the same `is_mutable_place` check and surfaces "no method
// `add_assign` on `u32`". That message buries the *real* operation
// (in-place add through a raw pointer); the user thinks AddAssign is
// somehow missing for u32. Compare with the equivalent `unsafe { *p
// = ¤u32_add(*p, 42); }`, which works fine — both paths mutate the
// same place.
//
// Why it's architectural: error layering. Typeck fails before
// safeck runs, so safeck's "raw-ptr deref needs unsafe" message
// never gets a chance to surface either way. Once problem 3 is
// fixed, this case will become a safeck-level rejection outside
// `unsafe` (correct) and a successful operation inside (correct).
//
// Fix shape: covered by the same `is_mutable_place` extension as
// problems 3 and 4.
#[test]
fn problem_5_compound_assign_raw_ptr() {
    expect_answer("redteaming/rt1/compound_assign_raw_ptr", 42i32);
}

// PROBLEM 6: `char` has no `PartialEq` impl in `lib/std/cmp.rs`,
// even though it's a Copy primitive with `as` casts in both
// directions to/from every integer kind. So `'a' == 'b'` errors
// "no method `eq` on `char`" — a minimal stdlib gap that's likely
// to bite anyone who tries to use chars in a real program.
//
// Why it's architectural-ish (vs. just a stdlib oversight): the
// pattern shows that `lib/std/`'s primitive impls are populated
// type-by-type with no audit step against "every Copy primitive
// should at minimum implement Eq/Ord". This will keep producing
// surprises (PartialOrd for `char`? Hash later? Display later?)
// until there's a checklist or a derive mechanism.
#[test]
fn problem_6_char_lacks_partial_eq() {
    expect_answer("redteaming/rt1/char_lacks_partial_eq", 42i32);
}

// PROBLEM 7: `&str` (and `str`) likewise have no PartialEq impl in
// `lib/std/cmp.rs`. `"hello" == "hello"` errors "no method `eq` on
// `&str`". String literals are typed `&'static str` and are the only
// way to construct strings today, so this gap means the language
// effectively has no string equality at all.
//
// Same architectural shape as problem 6: stdlib's primitive impls
// don't track DST refs (`&str`, `&[T]`) as equality-eligible
// primitives. Fixing it requires either explicit `impl PartialEq
// for str` (memcmp-shaped) or a more general
// `impl<T: PartialEq> PartialEq for [T]`-style blanket — and a
// decision about how str equality is implemented at the wasm level
// (memory-region compare).
#[test]
fn problem_7_str_lacks_partial_eq() {
    expect_answer("redteaming/rt1/str_lacks_partial_eq", 42i32);
}

// PROBLEM 9: bidirectional inference on a non-arithmetic user
// trait. `84.halve()` where `Halver` is a user trait with multiple
// Int-target impls (each with its own `Out` binding). Rust resolves
// this by deferring trait selection until the `let x: u32 = …`
// annotation pins the result type, then searching Halver's impls
// for one whose `Out = u32` — uniquely the `impl Halver for u32`
// row — and dispatching it. pocket-rust's num-lit dispatch path
// uses a hardcoded `numeric_lit_op_trait_paths()` list that doesn't
// include user traits, so the call surfaces "no method `halve` on
// `integer`".
//
// Why it's architectural: this is the same shape as problem 1, but
// without an arithmetic-trait-specific escape hatch. Whatever
// mechanism makes `30 + 12` typecheck under the user impl from
// problem 1 should also make this case work — anything else means
// "literal arithmetic is special-cased". Solving both with the same
// machinery (lazy projection + dynamic trait discovery) is the
// proper fix; the global `assoc_always_equals_self` collapse
// heuristic only addresses the arithmetic side.
//
// Fix shape: replace the hardcoded num-lit-trait list with a
// dynamic search for traits whose Int-target impls declare the
// method, and let the AssocProj-vs-concrete back-prop arm in
// `Subst::unify` resolve the dispatch when the result type is
// pinned by surrounding context.
#[test]
fn problem_9_bidirectional_user_trait() {
    expect_answer("redteaming/rt1/bidirectional_user_trait", 42i32);
}

// PROBLEM 8: at a generic-call site, the bound's assoc-constraint
// expected type (e.g. `T` in `T: Add<T, Output = T>`) is compared
// raw against the impl's actual binding without first being
// substituted under the call's inferred type-args. Calling
// `double::<u32>(21)` against
// `fn double<T: Add<T, Output = T>>(...)` looks up `impl Add for
// u32`'s `Output = u32`, then compares it against the unsubstituted
// constraint `T` and reports "type mismatch on associated type
// `Add::Output`: expected `T`, got `u32` (from `impl Add for u32`)"
// — telling the user that u32 doesn't satisfy a constraint that,
// after substitution, u32 trivially satisfies.
//
// Why it's architectural: every type-arg substitution in the bound
// system (impl-method validation, supertrait satisfaction) threads
// the inferred env through to the comparison. The call-site
// assoc-constraint check skips that step. The result is that the
// most natural Rust generic-arithmetic signature
// (`fn double<T: Add<T, Output = T>>`) doesn't compile.
//
// Fix shape: build an env mapping each template type-param to its
// inferred type, then `substitute_rtype(cty_expected, &env)` before
// the `rtype_eq` against the impl's binding.
#[test]
fn problem_8_assoc_constraint_not_substituted_at_callsite() {
    expect_answer(
        "redteaming/rt1/assoc_constraint_not_substituted_at_callsite",
        42i32,
    );
}
