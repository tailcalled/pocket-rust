---
name: typeck-pipeline
description: Use when modifying or debugging pocket-rust's type checker (`src/typeck/`). Covers the submodule layout, the resolved `RType` vocabulary, the inference machinery (`InferType`/`Subst`), integer-literal defaulting, and per-`Expr.id` typing artifacts that downstream passes consume.
---

# typeck pipeline

Entry: `pub fn check(&Module, &mut StructTable, &mut FuncTable, &mut next_idx) -> Result<(), Error>` in `src/typeck/mod.rs`. Constraint-based type inference.

## Resolved-type vocabulary

`RType` is the typeck-public vocabulary used by every downstream pass:

- `Int(IntKind)` — one of `u8`/`i8`/`u16`/`i16`/`u32`/`i32`/`u64`/`i64`/`u128`/`i128`/`usize`/`isize`.
- `Bool`, `Char`, `Str`, `Slice(T)`, `Never`.
- `Struct { path, type_args }`, `Enum { path, type_args }`, `Tuple(Vec<RType>)`.
- `Ref { inner, mutable, lifetime }`, `RawPtr { inner, mutable }`.
- `Param(name)` — a generic type parameter slot, used in registered impl/template signatures.
- `AssocProj { base, trait_path, name }` — an associated-type projection like `<T as Trait>::Name`.

`lifetime: LifetimeRepr` is `Named(String)` for a `'a` annotation, or `Inferred(u32)` for an elided/borrow-expression reference. Type equality and unification currently ignore the lifetime field — it's structural carry today and will start participating in checks in a later phase.

## Inference machinery

`InferType` mirrors `RType` but adds `Var(u32)` and `AssocProj` for unresolved holes. `Subst` is the substitution map; vars carry an `is_integer` flag so integer literals create fresh integer-class vars.

The walk collects `Eq`-style unification constraints (applied immediately) plus per-literal value/range constraints. Unifying an integer-class var with a non-integer concrete type fails immediately with `expected `X`, got integer`.

After body walk, any still-unbound integer-class var defaults to `I32` (Rust's convention); each literal's value is then range-checked against its resolved type.

## Per-function artifacts (NodeId-keyed)

Per-function typing artifacts live on `FnSymbol`/`GenericTemplate`, keyed by `Expr.id`:

- `expr_types: Vec<Option<RType>>` — one slot per Expr, holding its resolved type. Patterns also write here (typeck records each `Pattern.id`'s scrutinee type).
- `method_resolutions: Vec<Option<MethodResolution>>` — only `Some` at MethodCall ids.
- `call_resolutions: Vec<Option<CallResolution>>` — only `Some` at Call ids.

Downstream passes look up by id rather than maintaining source-DFS counters.

## Submodule layout (`src/typeck/`)

`mod.rs` re-exports the public surface, so external imports `crate::typeck::X` are unchanged.

- `types.rs` — `RType`, `IntKind`, `LifetimeRepr`; layout helpers (`byte_size_of`, `flatten_rtype`, `substitute_rtype`, `rtype_eq`, `rtype_to_string`); `is_copy`, `is_drop`.
- `tables.rs` — `StructTable`/`EnumTable`/`FuncTable`/`TraitTable` and their entries (`FnSymbol`, `GenericTemplate`, `MethodResolution`, `TraitDispatch`, `CallResolution`, `ReceiverAdjust`, `MoveStatus`/`MovedPlace`, `RTypedField`); simple `*_lookup` helpers.
- `traits.rs` — `ImplResolution`, `MethodCandidate`, `solve_impl`/`solve_impl_in_ctx`, `supertrait_closure`, `try_match_rtype`/`try_match_against_infer`, `find_method_candidates`, `find_trait_impl_method`, `find_trait_impl_idx_by_span`.
- `lifetimes.rs` — elision (`find_elision_source`), freshening, validation.
- `use_scope.rs` — `UseEntry`, `flatten_use_tree`, `ReExportTable`, `build_reexport_table`, `resolve_via_*`, the `*_lookup_resolved` reexport-aware lookup variants, and the visibility helpers (`is_visible_from`/`fn_defining_module`/`type_defining_module`/`field_visible_from`).
- `path_resolve.rs` — `resolve_type`, `resolve_full_path`, `lookup_variant_path`, `enum_lookup_resolved`, `place_to_string`, `segments_to_string`.
- `builtins.rs` — `BuiltinSig` + `builtin_signature` for the `¤` intrinsic table.
- `setup.rs` — every `collect_*`/`resolve_*`/`register_*`/`validate_*` pass that runs before body checking (struct/enum/trait/func collection, supertrait obligation enforcement, generic-impl validation).
- `methods.rs` — `check_method_call` and the symbolic-bound dispatch path. Receiver typing goes through `check_place_expr` (not the value-position `check_expr`) so the field-access "move out of borrow" gate doesn't fire on receivers that the dispatch will subsequently autoref. The Move-self case (consuming methods on through-ref non-Copy fields) gets caught by borrowck's `move_traverses_borrow` check on `MethodCall` operands instead. That borrowck check is intentionally narrow — it covers the one-projection-on-a-Ref-root case (`o.field.consume()`) and leaves multi-step chains (`(*o).p`, `o.x.p`) as a known gap (see `tests/gaps/borrowck.rs`).
- `patterns.rs` — `check_pattern`, struct/variant patterns, exhaustiveness (`exhausted`, `pattern_is_irrefutable`).
- `mod.rs` — entry `pub fn check`, `InferType`/`Subst`, `CheckCtx`, body walking (`check_module`, `check_function`, `check_block`, `check_expr` and the per-construct helpers).

## Integer literals

Integer literals are inferred from context. A bare `42` gets a fresh integer-class type variable; the variable unifies with whatever owns it (the let annotation, the param type at a call site, the field type in a struct literal, the function's return type, …). If no constraint pins the variable down, it defaults to `i32`. So `fn answer() -> u8 { 42 }` puts `42` into u8; `fn answer() -> i64 { 9_000_000_000 }` puts it into i64; `let x = 5` with no other use defaults `x` to i32.

**Type suffixes** — `42u32`, `100i64`, etc. — pin the literal's type at the source: the parser desugars `42u32` to `(42 as u32)` at parse time, threading through the existing cast machinery. Recognized suffixes are exactly the int-kind names (`u8`/`i8`/.../`u128`/`i128`/`usize`/`isize`).

## Type checking rules

- Every call's arguments must match the callee's parameter types.
- Every struct-literal field initializer must match its declared field type.
- A function's tail expression must match the declared return type.
- Field access on a non-struct value (`expr.field` where `expr` is `usize`) is rejected.
- Duplicate function/struct paths aren't detected; the relevant lookup returns the first match.

## Block scoping

Each pass scopes a block expression by saving `locals.len()` (typeck/codegen) before entering and truncating back on exit, so let bindings inside a block aren't visible outside. The WASM locals allocated for those bindings remain in the function (we don't reuse local slots across scopes), but they're harmlessly unreferenced after the block ends. CFG construction emits matching `StorageLive`/`StorageDead` markers around block-scoped bindings; the move analysis uses StorageDead points to compute per-binding scope-end move-state.
