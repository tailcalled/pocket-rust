---
name: patterns-and-matching
description: Use when working with patterns, `match`, `if let`, `let-else`, or destructuring. Covers pattern AST, refutability, exhaustiveness, codegen lowering (`codegen_pattern`), `PatScrut` storage variants, and divergence detection for let-else.
---

# patterns and matching

## Pattern AST

`Pattern { kind: PatternKind, span, id }`. The `id` is a per-fn `NodeId` allocated by the parser on the same counter as `Expr.id`. Typeck records each pattern's resolved scrutinee type at `expr_infer_types[pattern.id]` so codegen can read the layout directly.

`PatternKind`:
- `Wildcard` — `_`
- `LitInt(i64)` — integer literal
- `LitBool(bool)` — `true` / `false`
- `Binding { name, mutable, by_ref, name_span }` — `name` / `mut name` / `ref name` / `ref mut name`
- `Ref { inner, mutable }` — `&pat` / `&mut pat`
- `Tuple(Vec<Pattern>)` — `(p, q)` (same `()`/`(p,)`/`(p, q)` rules as tuple expressions)
- `VariantTuple { path, elems }` — `Path(p, q)`
- `VariantStruct { path, fields, has_rest }` — `Path { f, g: pat, .. }` with shorthand and `..`-rest
- `Or(Vec<Pattern>)` — `p | q` (alternatives must bind the same names with unifiable types)
- `Range { lo, hi, inclusive }` — `lo..=hi`
- `At { name, inner, name_span }` — `name @ pat` at-bindings

Variant references must use a path (`E::A`) — bare-ident patterns are always bindings, so a unit variant referenced bare-ident is a (shadowing) binding, not a variant match.

## Type checking

`check_pattern(ctx, pattern, scrutinee_ty, bindings)` is the public entry; internally it threads a default `BindingMode` (Move/Ref/RefMut) through `check_pattern_with_mode` for match-ergonomics descent (RFC 2005). Each call recursively unifies the scrutinee type against the pattern shape, collecting `(name, ty, span, mutable)` bindings into the arm scope. Bindings introduced by Or-patterns must agree across alternatives — checked by walking the first alt's bindings and unifying each subsequent alt's set.

## Match ergonomics

When a non-reference pattern is matched against a `&T` / `&mut T` scrutinee, `check_pattern_with_mode` auto-peels ref layers and bumps the default binding mode (Move → Ref/RefMut). Bindings descended-to under a non-Move mode bind by reference even when the AST has `by_ref: false`. Explicit `&pat` resets the mode to Move (the user is stripping the ref themselves). Mode bumps are demotion-monotone: once at Ref, peeling another `&mut` keeps the mode at Ref (you can't get exclusive access through an outer shared borrow).

Decisions are recorded per pattern.id in `ctx.pattern_ergo: Vec<PatternErgo>` (see `src/typeck/tables.rs`), finalized into `FnSymbol.pattern_ergo` / `GenericTemplate.pattern_ergo`:
- `peel_layers: u8` — how many ref layers were auto-peeled at this pattern node.
- `peel_mut_bits: u8` — bit i set if the i-th outermost peel was `&mut`.
- `binding_override_ref` / `binding_mutable_ref` — for Binding/At nodes whose effective mode differs from the AST.

The original AST is never mutated — `pattern_ergo` is a side table that downstream passes consult.

**Downstream consumption:**
- **Borrowck** (`build.rs`): `lower_pattern_test` + `collect_bindings_into` peel the scrutinee place via `Projection::Deref` and strip ref layers off the type, then dispatch the pattern's kind. Binding overrides flow into `PatternBinding.by_ref` / `mutable`.
- **Mono** (`mono.rs`): `desugar_pattern` walks the AST + `pattern_ergo` and produces a fresh AST pattern with explicit `Ref { ... }` wrappers and `Binding { by_ref: true, ... }`. Codegen sees the explicit-form pattern and uses its existing `Ref` / `ref-binding` paths — no codegen-side ergonomics knowledge required.
- **Exhaustiveness** (`gather_ref_inner_pats`): non-Ref patterns count as inner ref-pattern coverage when the scrutinee is a reference, so `match o: &Option<u32> { Some(_) => ..., None => ... }` is exhaustive.

Currently covered shapes: `Wildcard`, `Binding`, `At`, `VariantTuple`, `VariantStruct`, `Tuple`, `Or`. `LitInt`/`LitBool`/`Range` don't auto-peel yet (would need the literal-pattern handler to dereference); `match &5 { 5 => ... }` still requires explicit `&5`.

## Exhaustiveness

`check_match_exhaustive(ctx, scrutinee_ty, arms)` is enforced structurally:
- Enums: every variant covered (unconditional pattern, or per-payload-position recursive coverage).
- Booleans: both `true` and `false` arms.
- Tuples: every position covered.
- Integers/refs/structs: an unconditional arm.

`exhausted(ctx, ty, arms)` is the recursive workhorse. `check_match_exhaustive` substitutes the enum's type-args into payload types when recursing so generic enums (`Option<Option<T>>`) check correctly.

## Refutability — `pattern_is_irrefutable`

Defined as `exhausted(ctx, ty, &vec![pat])` — a pattern is irrefutable iff it alone exhausts the scrutinee type. Reuses the same machinery as match exhaustiveness so the rules stay coherent. Drives:
- `let PAT = …;` requires PAT to be irrefutable (else "refutable pattern in `let`").
- `let PAT = … else { … };` permits refutable PAT.

## `match` codegen

Lowers to a wasm `block` chain:
- Outer block carries the match's result type (`BlockType::Single(vt)` for one wasm scalar, `TypeIdx(i)` for multi-value, `Empty` for unit).
- Each arm sits in an inner empty-result block whose pattern check `br_if`s out on mismatch.
- On a match the arm body executes and `br 1`s to the outer with its result.
- After all arms, an `unreachable` guards the validator's "block must produce a result" rule.

Scrutinee storage is dispatched by type via `PatScrut`:
- `PatScrut::Locals { start }` — for non-enum types (flat scalars stashed into wasm locals).
- `PatScrut::Memory { addr_local, byte_offset }` — for enum types (the value's bytes live at `[addr + offset]`, since enums are address-passed).

Pattern recursion advances `byte_offset` for memory storage and `flat_offset` for locals.

## `codegen_pattern` per-shape

- **Variant-tuple/struct patterns:** load the disc at offset 0, compare to the variant's expected disc, `br_if`-on-mismatch, then recurse into payload fields. Hardcoded discriminant order matches declaration order in the enum.
- **Or-patterns:** nest extra wasm blocks so each alt can fall through to the next.
- **Ident bindings:** copy/load the matched value into fresh wasm locals owned by the binding (or, for enum bindings, cache the address as `Storage::Local { wasm_start }` of size 1 holding the i32).
- **At bindings:** push the outer At binding then recurse into inner.
- **Addressed bindings:** if `pattern_addressed[pattern.id]` is true (set by escape analysis or by `auto_address_drop_pattern_bindings`), allocate a shadow-stack slot up front (`__sp -= byte_size_of(ty)`), copy bytes from the scrutinee storage, and register the binding as `Storage::MemoryAt { addr_local }`. This is what makes Drop pattern bindings drop correctly at scope end and what lets `&binding` borrows work on pattern-bound names.

Borrowck snapshots `state.moved` per arm and merges via `merge_moved_sets` to track moves through pattern-matched paths.

## `if let` codegen (E4)

`if let Pat = scrut { then } else { else }`. First-class AST node (not desugared to `match` — keeps the surface form for diagnostics). The `else` is parser-optional and defaults to an empty unit block. Typeck unifies the two arms to a common type; pattern bindings scope to the then-block only.

Codegen mirrors a single-arm match: outer wasm `block` (result type T), inner empty-result `block` for the pattern check; on no-match `BrIf 0` skips the then-block and falls through to the else-block; on match the then-block runs and `Br 1` escapes past the else with its result. Borrowck snapshots pre-state and walks both arms, merging `state.moved` via `merge_moved_sets`.

## `let-else` codegen

`let PAT = EXPR else { DIVERGING };`. Pattern bindings are not in scope inside the else block; they scope to the rest of the enclosing block on the match path.

`codegen_let_else` mirrors `if-let`: outer Block + inner Block, `codegen_pattern` `br_if`s out on no-match, the matched path runs `Br 1` to escape past the else block, the no-match path runs the diverging else then `Unreachable`.

```
<codegen value into a stashed scrutinee>
block (Empty)            ; outer — control resumes here on match
  block (Empty)          ; inner — pattern-test br_if-out lands here
    <codegen_pattern>    ; on no-match, br 0 (out of inner)
    <bindings live>      ; pushed onto ctx.locals by codegen_pattern
    br 1                 ; matched — skip past the else block
  end                    ; no-match path lands here
  <codegen else block>   ; diverges (typeck-enforced)
  unreachable
end                      ; resumes here only on match
```

## let-else divergence check

The else block must diverge: typeck checks that either the block's tail expression has type `!` or some statement in the block does. `block_has_diverging_stmt(ctx, block)` walks `block.stmts`, looks at each `Stmt::Expr(e)` and reads `expr_infer_types[e.id]`; if any resolves to `Never`, the block diverges.

**Type-driven, not AST-shape-driven** — so any future `!`-typed expression (calls to `!`-returning functions, etc.) is recognized automatically without enumerating ASTNode kinds. This is what lets `let Pat = … else { return 0; };` work even though the inner `return` carries a trailing `;`.

## Pattern restrictions

- For-loop pattern is currently restricted to `Var(name)` or `_`; destructuring patterns inside `for` panic at borrowck (TODO — needs pattern lowering).
- Tuple destructure in `let` supports leaf Binding/Wildcard sub-patterns; nested destructure inside tuple (e.g. `let ((a, b), c) = …;`) is a codegen TODO.
- Guards (`pat if cond => arm`) are reserved in the AST but rejected at typeck.
