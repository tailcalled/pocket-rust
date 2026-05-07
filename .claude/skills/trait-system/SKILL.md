---
name: trait-system
description: Use when working with traits, impls, generics, method dispatch, associated types, supertraits, default trait params, or generic-trait params + deferred dispatch. Covers everything about how `trait` / `impl` declarations are resolved and how `recv.method()` calls pick a callee.
---

# trait system

## Generics

`fn id<T>(x: T) -> T { x }` for generic functions; `struct Pair<T, U> { first: T, second: U }` for generic structs; `impl<T, U> Pair<T, U> { fn ... }` for generic impls. Inference works (`id(5)` → `id<i32>`); turbofish for explicit args (`id::<u32>(5)`, `Pair::<u32, u64>::new(...)`, `recv.method::<U>(arg)`). Type-position uses `<>` directly (`Pair<u32, u64>`); expression-position uses turbofish `::<>` (`Pair::<u32, u64> { ... }`).

Generic params are unbounded except for an **implicit `T: Sized` bound** (no `?Sized` syntax yet) — DSTs (`str`, `[U]`) cannot bind to `T` at any "sized" position. Sized is enforced positionally: when `try_match_rtype` / `try_match_against_infer` recurse into a `Ref`/`RawPtr` they flip off the Sized check (so `impl<T> Copy for &T` matches `&str`/`&[U]`), but bind sites at outer / Tuple / Struct / Enum positions still require Sized. Method-dispatch additionally enforces Sized per impl by walking the impl_target with `collect_sized_required_params` to find every type-param appearing outside Ref/RawPtr — that's the set whose env binding must be Sized at use sites. The combined effect: `impl<T> Trait for T` rejects `T = str`, `impl<T> Copy for &T` accepts `T = str`, `impl<T> Trait for Vec<T>` rejects `T = str`.

Without bounds, the only operations on a `T` value are move, pass as arg/return, and take `&t` / `&mut t`. Field access on `T`, calls on `T`, casts to `T`, and `T { ... }` literals are rejected at the polymorphic body check (typeck runs once on each generic body with `RType::Param("T")`). The inner of a `&...` / `&mut...` is type-checked as a *place expression* via `check_place_expr`, which walks `Var` / `FieldAccess` / `Deref` chains structurally without applying the value-position "non-Copy through ref" move-out rule — so `&self.field` works for any field type. Reading the same chain in value position (`let v = self.field;`) still applies the rule.

Struct lit types are recorded per-`StructLit` in source-DFS order on `FnSymbol.struct_lit_types` / `GenericTemplate.struct_lit_types` so codegen can lay out generic struct literals using the inferred type args.

## Impl targets

Impl targets accept arbitrary patterns: `impl<T> Pair<usize, T>` (partial concrete), `impl<T> Pair<T, T>` (repeat param), `impl Pair<u32, u64>` (fully concrete). Each impl's resolved target lives on `FnSymbol.impl_target` / `GenericTemplate.impl_target` as an `RType` pattern (with `Param("T")` slots for impl type-params).

For trait impls on non-Path targets (`impl Trait for (u32, u32)`, `impl<T> Trait for &T`, `impl Trait for bool`), typeck synthesizes the method-path prefix `__trait_impl_<idx>` where `idx` is the impl's row in `TraitTable.impls`. Codegen recovers that idx via `find_trait_impl_idx_by_span(traits, file, span)`.

## Trait declarations

`trait Name { fn method(...); ... }` declares a trait with method signatures (no default bodies). `impl Trait for Target { fn method(...) {...} ... }` provides an implementation. Trait paths and impl rows live on `TraitTable` (entries + impls); registration validates that every declared method is covered with no extras, and rejects two `impl T for Pat` rows whose `(trait_path, target)` `rtype_eq`.

Bounds: `<T: Trait1 + Trait2>` parse and resolve. Each fn/impl's per-type-param resolved bound trait paths live on `GenericTemplate.type_param_bounds` and `TraitImplEntry.impl_type_param_bounds`. Impl targets accept any type pattern: `impl<T: Show> Show for &T` and `impl<T> Show for Wrap<T>` both work; inherent (non-trait) impls still require the target to resolve to a struct.

Where-clauses: `fn f<T>() -> R where T: Trait1, Vec<T>: Trait2 { ... }` parses on Function, TraitMethodSig, and ImplBlock (parser appends `where_clause: Vec<WherePredicate>` after the `-> R` and before the body brace). Setup splits each predicate by LHS shape:
- **Bare type-param LHS** (`T: Bound`) — appended onto the matching type-param's rows (`type_param_bounds` / `type_param_bound_args` / `type_param_bound_assoc`), so it's indistinguishable from an inline `<T: Bound>` bound from this point on.
- **Complex LHS** (`Vec<T>: Bound`, `&T: Bound`, `(T, U): Bound`, …) — stored on `GenericTemplate.where_predicates: Vec<WherePredResolved>` (or rejected at setup with "predicate not satisfied" if the function is non-generic and the LHS has no impl). At each call site, after the type-param substitution `subst_env` is built, the LHS is substituted and each bound is checked via `solve_impl_in_ctx_with_args`. Failure → "where-clause predicate not satisfied at call site".

## Trait method signatures

Each `TraitMethodEntry` records the resolved `param_types: Vec<RType>`, `return_type: Option<RType>`, `receiver_shape: Option<TraitReceiverShape>` (Move/BorrowImm/BorrowMut), and `type_params: Vec<String>` (method-level `<U>` names).

`resolve_trait_methods` (after struct fields, before func registration) walks each `Item::Trait` and resolves method signatures with `Self` as `RType::Param("Self")` and the method's own `<U>` as `RType::Param("U")`.

`validate_trait_impl_signatures` (after impl methods are registered) requires the impl method's method-level type-param arity to match the trait's, then substitutes `Self → impl_target` and each `U_i → Param("__trait_method_<i>")` on both sides (α-equivalence) before `rtype_eq`-ing the param and return types.

Symbolic dispatch through bounds (`check_method_call_symbolic`) uses the trait method's signature for arg type-checking and return-type propagation, derives `recv_adjust` from the receiver shape, and — when the trait method has its own type-params — allocates a fresh inference var per param (or pins them via turbofish `t.bar::<u32>(arg)`). The resolved method-level type-args land on `MethodResolution.type_args`; codegen's `codegen_trait_method_call` fills the impl method template's impl-level slots from `solve_impl(...).subst` and the method-level slots from `MethodResolution.type_args` (substituted through the outer mono env).

## Supertraits

`trait Sub: Super1 + Super2 { ... }` declares `Sub` as a refinement of its supertraits. `TraitDef.supertraits: Vec<TraitBound>` carries the parsed bounds; `resolve_trait_methods` resolves each via `resolve_trait_ref` (paths + concrete arg types) and stores them on `TraitEntry.supertraits: Vec<SupertraitRef>` where each entry has `path: Vec<String>` plus `args: Vec<RType>` (the args reference the trait's own type-params and `Self`). `validate_supertrait_obligations` substitutes the impl's `trait_args` (and `Self → impl_target`) into each supertrait's args before calling `solve_impl_in_ctx_with_args`. Without arg-aware supertrait checks, `IndexMut<Range<usize>>: Index<Range<usize>>` would resolve against `Index<usize>` instead. `supertrait_closure(start, traits)` returns `[start] + transitive supertraits`, deduplicated (cycles break naturally on the dedup check). Supertrait `Self::Output` references in `IndexMut`'s body are resolved through `walk_resolve_self_proj`'s supertrait-aware branch — when a projection's trait_path is a supertrait of the trait being checked, it substitutes the impl's trait_args into the supertrait's args and looks up the matching impl row via `find_assoc_binding_with_args`.

Three places consume the closure:
1. `validate_supertrait_obligations` — run after all impls are registered; requires that every `impl Sub for T` row has matching `impl Super for T` rows for each supertrait. Uses `solve_impl_in_ctx` so generic impls like `impl<T: PartialEq> Eq for Wrap<T>` are satisfied by the matching generic PartialEq impl.
2. `solve_impl_in_ctx` (the bounded form of `solve_impl`, with optional in-scope `(type_params, type_param_bounds)` context) — treats `Param(name)` as satisfying any trait in `name`'s bounds' supertrait closure. `is_copy_with_bounds` and `satisfies_num` both delegate to it.
3. `check_method_call_symbolic` — walks each bound's supertrait closure when looking for the method on `<T: Bound>` recv, so `<T: Eq>` reaches `PartialEq::eq` and `<T: Ord>` reaches `PartialOrd::lt`.

Stdlib exploits this: `Eq: PartialEq {}` and `Ord: PartialOrd + Eq {}` are pure marker traits — every primitive provides `impl PartialEq + impl Eq + impl PartialOrd + impl Ord`, with PartialEq/PartialOrd carrying the actual method bodies.

## Associated types

A trait can declare `type Name;` items (no defaults, no bounds yet); each impl provides `type Name = ConcreteType;` bindings. `TraitDef.assoc_types` and `ImplBlock.assoc_type_bindings` carry the AST; `TraitEntry.assoc_types` (names only) and `TraitImplEntry.assoc_type_bindings: Vec<(String, RType)>` carry the resolved form.

`resolve_and_validate_assoc_bindings` enforces: no duplicates, no extras (must be a member of the trait), no missing (every declared name has a binding); inherent impls aren't allowed to declare any.

References use `Self::Name` (inside trait/impl scopes) or `T::Name` (where T is an in-scope type-param) — the parser keeps these as 2-segment paths; `resolve_type` detects the case at typeck and emits `RType::AssocProj { base, trait_path, name }` (mirror `InferType::AssocProj` for inference). The `trait_path` field is left empty by `resolve_type`; lookup is by `(base, name)` — disambiguation by trait is by which trait declares the assoc, and ambiguity surfaces if multiple do.

`concretize_assoc_proj_with_bounds` walks an `RType` replacing each `AssocProj` with the matching impl's binding (via `find_assoc_binding`) or with the in-scope `T: Trait<Name = X>` constraint type — leaves it unresolved if neither succeeds. The InferType counterpart `infer_concretize_assoc_proj` does the same for dispatch result types. Concretization runs at: impl-method registration (so the registered `param_types`/`return_type` carry no AssocProj), trait-vs-impl signature comparison (substitute `Self → impl_target` then concretize), `check_function`'s body-checking entry (re-resolves param/return types against the function's bounds), and the symbolic-dispatch return-type computation.

Bound constraint syntax: `Trait<Name = Type, ...>` parses inside any `TraitBound`; `TraitBound.assoc_constraints: Vec<AssocConstraint>` carries them. Each function's bounds' resolved constraints are computed at body-check time and threaded through `CheckCtx.type_param_bound_assoc` for use by `infer_concretize_assoc_proj`; they also live on `GenericTemplate.type_param_bound_assoc`.

**Static enforcement at call sites:** `check_call`'s template branch, after arg unification has bound the call's type-vars to concrete types, walks each type-arg's bounds. For every `<Name = X>` constraint on a bound, `find_assoc_binding(traits, inferred_T, trait_path, Name)` looks up the impl's actual binding and rejects the call when the binding is missing or doesn't `rtype_eq` the constraint's `X` — surfaced as "type mismatch on associated type `Trait::Name`: expected …, got …". Currently scoped to free function calls; the analogous check on method-level type-params with assoc-constraint bounds is a follow-up.

## AssocProj back-propagation (for operator traits)

The operator traits all declare `type Output;`; trait method signatures return `Self::Output`. When the call result `<? as Add>::Output` reaches the function's return-type unification, two mechanisms combine to pin the result:

1. **Self::Output ⇒ Self collapse for "Output = Self" traits.** `infer_concretize_assoc_proj` checks via `assoc_always_equals_self(trait_path, name)` whether *every* registered impl of `trait_path` binds `name` equal to its target. If so (the case for primitive impls of Add/Sub/Mul/Div/Rem/Neg), the projection collapses to its base. So `<? as Add>::Output` reduces to `?` and inference proceeds normally — chained ops like `1 + 2 + 3` and unary on op-result like `-(30 + 12)` work without Output blocking dispatch on the intermediate result.

2. **AssocProj-vs-concrete unification.** When `<Var as Trait>::Output` unifies with a concrete `T`, the unifier walks `Trait`'s impls and finds the unique impl whose `Output = T`. If exactly one matches, it binds Var to that impl's target. So `30 + 12` where the function returns `u32` pins the recv Var to u32 even though the literal would otherwise default to i32.

At end-of-fn typeck finalize, for each `PendingTraitDispatch` whose recv is concrete and trait_args were left as defaulted Vars, the finalizer prefers the unique impl matching `(trait_path, recv_type)` and adopts its trait_args.

## Default trait-level type params

`trait Foo<X, Y = Self>` declares a default for `Y`; impl/bound sites can omit trailing args and the resolver fills them in. `TypeParam.default: Option<Type>` carries the AST; `TraitEntry.trait_type_param_defaults: Vec<Option<RType>>` carries the resolved defaults (per slot, `None` if no default was written).

At each `resolve_trait_ref` call site, missing trailing args (provided.len() < total_params.len()) are filled by substituting the default through an env that maps `Self → self_target` (the impl's target type at `impl Trait for Foo`, the bound holder at `T: Trait`) plus earlier slots' resolved values. A trailing slot with no default is rejected at use site with "missing type argument for trait `…`: parameter `…` has no default". Only trait-level params support defaults today.

## Generic-trait params + deferred dispatch

`trait Foo<X, Y>` declares positional trait-level type-params (`TraitEntry.trait_type_params`); `impl Foo<u32, i64> for Bar` resolves them to `TraitImplEntry.trait_args: Vec<RType>`. `solve_impl_with_args(trait_path, trait_args, concrete_recv, …)` keys on both target *and* trait_args, so multiple `impl Mix<X> for Foo` rows coexist.

Method registration appends `__trait_impl_<idx>` to the prefix when the trait has type-params (otherwise the rows would collide at `[…, Foo, mix]`). Setup, typeck, borrowck, and codegen all derive that idx via `find_trait_impl_idx_by_span`.

At a call site with multiple matching candidates from the same generic trait (e.g. `Foo{}.mix(0)` matching both `Mix<u32>` and `Mix<i64>` impls), `check_method_call` switches from "pick a candidate now" to **deferred dispatch through the trait's signature**: fresh inference vars for each trait-arg slot, args type-checked against the trait method's substituted signature, vars left to be pinned by surrounding usage (a later `let y: u32 = x;`, etc.). At end-of-function, `subst.finalize` resolves the trait_arg vars; once concrete, the typeck driver runs `solve_impl_with_args` to verify a matching impl exists and emits `no impl of `Trait<args>` for `Recv`` otherwise (driven by `PendingTraitDispatch.dispatch_span`). Concrete trait_args land on `MethodResolution.trait_dispatch.trait_args`, which codegen passes to `solve_impl_with_args` to pick the wasm idx.

The defer-only-when-trait-has-params guard keeps the existing "two overlapping impls of a non-generic trait" case as an ambiguity error.

## Method dispatch — receiver-type chain

`recv.method()` collects every method-shaped FuncTable entry/template named `method` via `find_method_candidates` (no impl-target shape filter). Dispatch then mirrors rustc's receiver-type-based resolution rather than matching impl patterns directly.

For each candidate (impl, method) the **effective receiver type** is `subst(method.params[0], Self → impl_target)` — already substituted at impl-method registration in `setup.rs`, so this is just the raw `param_types[0]`. A candidate-self-type chain is built from the recv: `[recv_full, &recv_full, &mut recv_full]` (the `&mut` level is omitted when `recv` isn't a mutable place; the deref level is not implemented).

Walk the chain in order; at each level, try to unify every candidate's effective recv type against the level. First level with at least one match wins; the level chosen drives `recv_adjust` directly:
- level 0 → `ByRef` if recv is a Ref else `Move`
- when recv is `&mut T`, also try `&T` at the same level → `ByRef` (mut→shared downgrade, ABI no-op since both refs are i32). Mirrors Rust's auto-reborrow rule: a `&self` method called on a `&mut T` binding is implicitly downgraded to `&T`.
- `&recv_full` → `BorrowImm`
- `&mut recv_full` → `BorrowMut`

Multiple matches at the same level → "ambiguous method `m` on `T`: multiple impls match" (no specialization implemented).

**Implicit `T: Sized` enforcement:** for impls with type-params (`impl<T> ...`), `collect_sized_required_params` walks the impl_target and finds every Param appearing outside any `Ref`/`RawPtr` wrapper — those positions need a known compile-time size (e.g. `impl<T> Trait for T`, `impl<T> Trait for Vec<T>`, `impl<T> Trait for (T, u32)`). After a candidate matches at a level, the env binding for each such Param must be Sized; bindings to `str`/`[U]` are rejected. Params that appear *only* inside Ref/RawPtr (e.g. `impl<T> Copy for &T`) are exempt — mirror of Rust's `impl<T: ?Sized> Copy for &T` opt-out (which pocket-rust doesn't yet have syntax for).

Together these handle: `s: &str; s.test()` with `impl MyTrait for str` + `impl<T> MyTrait for T` resolves to the str impl at level `&str` (blanket excluded by Sized since T would be `str`), matching rustc; `r: &u32; r.show()` with `impl Show for u32` + `impl<T> Show for &T` picks the u32 impl at level `&u32` (its method recv type is `&u32`); the blanket's `&&T` recv type would only match at level `&&u32`, later in the chain.

If `recv` is `Param(T)` (or `&Param(T)`/`&mut Param(T)`), dispatch goes through `T`'s bounds — exactly one bound trait must declare the method, otherwise "no method/ambiguous" (handled in `check_method_call_symbolic`'s symbolic path before the chain walk).

Trait-dispatched calls store a `TraitDispatch { trait_path, method_name, recv_type }` on the `MethodResolution`; codegen substitutes `recv_type` against the mono env, runs `solve_impl(trait_path, concrete_recv)` (recursive, depth-bounded; checks `where T: Bound` constraints by recursing), and emits a call to the resolved impl method's wasm idx (interning the mono key when the method is generic). Recursive impl resolution handles nested cases: `Wrap<Wrap<u32>>: Show` matches `impl<T: Show> Show for Wrap<T>` twice and `impl Show for u32` once, producing three distinct monomorphized `show` functions. Trait-dispatched call codegen currently assumes `Move` receiver adjust (consuming `self`); fuller `&self`/`&mut self` handling for trait dispatch is a follow-up.

## Methods (inherent)

`impl Type { fn method(...) {...} ... }` defines inherent methods on a struct, enum, or raw pointer (`*const T` / `*mut T`). For struct/enum targets the path's first AST segment becomes the method-table prefix; for raw-pointer targets, setup allocates a synth idx (recorded as `(file, span)` in `FuncTable.inherent_synth_specs`) and the prefix is `__inherent_synth_<idx>` — body-check and codegen recover the same idx via `find_inherent_synth_idx`. Refs, primitives, and tuples can only carry methods through trait impls.

Receivers are `self`, `&self`, or `&mut self`, desugared at parse time to a regular `self: Self` / `self: &Self` / `self: &mut Self` first param. `Self` resolves to the impl target's type (in both type and path positions — `Self::new(…)` and `Self { x: 1 }` work).

Method calls `recv.method(args)` autoref the receiver based on the method's declared receiver type: owned recv → `&Self`/`&mut Self` method takes its address (recv must be a place; `&mut Self` requires a mutable place); `&` recv → `&Self` method passes through; `&mut` recv works for both `&Self` (downgrade) and `&mut Self`. Calling a `&mut self` method through `&T` is rejected; calling a by-value `self` method on a borrowed receiver is rejected.

UFCS works through normal path resolution: `Type::method(recv, args)` looks up methods registered as `[..., Type, method]` in the FuncTable. Methods are *not* exported from the WASM module under their bare name — only crate-root free functions are.

## `Copy` (built-in marker trait)

Defined as `pub trait Copy {}` in `lib/std/marker.rs` (re-exported as `std::Copy`), with `impl Copy for {u8, i8, …, isize, bool, char} {}`, `impl<T> Copy for &T {}` (shared refs only — `&mut T` is exclusive and not Copy), and `impl<T> Copy for *const T {}` / `impl<T> Copy for *mut T {}`.

The compiler treats the canonical path `["std", "marker", "Copy"]` specially via `copy_trait_path()` and `is_copy(rt, traits)`, which dispatches through the standard `solve_impl(Copy, rt)` resolver — no special-case code paths.

User code can `impl Copy for SomeStruct {}`; both concrete and generic-target Copy impls validate that every field is Copy. The generic case routes through `is_copy_with_bounds` which recognizes `RType::Param("T")` as Copy when the bound list includes Copy — so `impl<T: Copy> Copy for Wrap<T> {}` passes while `impl<T> Copy for Wrap<T> {}` is rejected.

Trait paths in user code are resolved via `resolve_trait_path`, which consults the active use scope before falling back to module-relative or absolute lookup. Unqualified `Copy` reaches `std::Copy` because `compile` auto-injects `use std::*;` at the user crate root for any `Library` with `prelude: true`.

## `Drop` / `Copy` mutual exclusion

`register_trait_impl` rejects an impl of one when the other already exists for the same target. See the `drop-and-destructors` skill for full Drop machinery.

## Numeric literal codegen (post-Num)

With literal overloading dropped, every integer literal resolves to `Int(kind)` after typeck. `emit_int_lit` lowers it to a single `i32.const` / `i64.const` / two-`i64.const` (wide128) — no trait dispatch, no `from_i64` call. The literal-class type-var (`Subst.is_num_lit`) still exists so `1.add(2)` can dispatch through trait Add even before the var is pinned: `bind_var` rejects non-Int / non-Var candidates (`satisfies_num` only accepts `Int(_)` and `Var(_)` now), and the var defaults to `i32` at body-end if unconstrained.
