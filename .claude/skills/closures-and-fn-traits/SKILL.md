---
name: closures-and-fn-traits
description: Use when working with closure expressions (`|args| body`, `move |args| body`, `||` no-arg), the `Fn`/`FnMut`/`FnOnce` trait family, the parenthesized `Fn(T) -> R` trait sugar, higher-ranked trait bounds (`for<'a>`), capture analysis, or anything else about treating an anonymous code-and-environment value as a callable.
---

# closures and Fn traits

Closures work end-to-end: parse, type-infer (with bidirectional inference from `Fn(A)->R` bounds), capture analysis (Copy by-value, non-Copy by-ref, mutating → RefMut), lower to anonymous struct + Fn/FnMut/FnOnce impls, dispatch via `f.call(...)` or bare `f(args)` sugar. Tests in `tests/lang/closures.rs` (31 cases) exercise the surface end-to-end with runtime invocation.

## Trait family in `lib/std/ops.rs`

```
pub trait FnOnce<Args> { type Output; fn call_once(self, args: Args) -> Self::Output; }
pub trait FnMut<Args>: FnOnce<Args> { fn call_mut(&mut self, args: Args) -> Self::Output; }
pub trait Fn<Args>: FnMut<Args> { fn call(&self, args: Args) -> Self::Output; }
```

`Args` is always a tuple at the use site; the parenthesized sugar `Fn(T1, T2) -> R` rewrites to `Fn<(T1, T2), Output = R>` at parse time. `Output` is declared on `FnOnce` only and inherited via the supertrait chain.

## Parser surface

**Closure expression — `parse_closure` (sits at parse_atom level):**
- `|p1, p2| body` — comma-separated params inside `|...|`. Each param is an irrefutable pattern, optionally annotated: `name`, `_`, `(a, b)`, `Foo { x, y }`, `&pat`, …, with optional `: T`. Refutability is checked at typeck time against the param's (possibly inferred) type and reuses the let-binding/match-exhaustiveness machinery.
- `|| body` / `move || body` — empty arg list, parsed from the `OrOr` token (the only place a leading `||` can't be the logical-or operator).
- `move |args| body` — `move` is its own `TokenKind::Move`.
- Optional `-> R` after `|...|` makes the body a brace-block: `|x: u32| -> u32 { x + 1 }`. Without `-> R`, body is `parse_expr()` — extends right as far as expression precedence allows.

**Trait-bound sugar:** `Path(T1, T2) -> R` after the path rewrites to `Path<(T1, T2), Output = R>`. Empty arg list = `()`. Absent `-> R` → `Output = ()`. The parenthesized form precludes `<…>`.

**HRTB:** `for<'a, 'b> Path<...>` parses with the lifetimes captured on `TraitBound.hrtb_lifetime_params`. Lifetimes scope only into the bound's own types. Setup's bound resolution loop validates each resolved trait-arg's named lifetimes against the enclosing fn/impl scope **plus** the bound's HRTB lifetimes — so `fn f<F: for<'a> Fn(&'a u32) -> u32>` accepts `'a` while `fn f<F: Fn(&'a u32) -> u32>` (no HRTB) is rejected with `undeclared lifetime 'a`.

**APIT (argument-position `impl Trait`):** `fn apply(f: impl Fn(u32) -> u32) -> u32` parses to `TypeKind::ImplTrait(bounds)` and the parser's post-params `desugar_apit` step rewrites it into a fresh anonymous type-param `__impl_<n>` with the recorded bounds, appended to `Function.type_params`. Calls dispatch the same way as an explicit `<F: Fn(...)>` declaration. Bare-call `f(args)` against an APIT-typed local routes through `check_bare_typeparam_fn_call` (sibling of `check_bare_closure_call`): looks up the Fn signature on the type-param's bound via `typeparam_fn_signature` (reads `ctx.type_param_bounds` / `type_param_bound_args` / `type_param_bound_assoc`), unifies the arg tuple, and records a `Fn::call` trait dispatch with `recv_type = Param(name)`. Return-position `impl Trait`, struct-field `impl Trait`, and other non-fn-arg positions are rejected at `resolve_type` with "`impl Trait` is only allowed in argument position".

## AST

- `ExprKind::Closure(Closure)` — `Closure { params, return_type: Option<Type>, body: Box<Expr>, is_move: bool, span }`.
- `ClosureParam { pattern, ty: Option<Type> }` — `pattern` is an irrefutable AST `Pattern` (commonly a `Binding`, but tuple destructure / wildcard / ref-pattern all parse). `ty: None` means inferred from context.
- `TraitBound.hrtb_lifetime_params: Vec<LifetimeParam>` — empty for ordinary bounds.

## Architecture: type-driven lowering after typeck

Closures are **first-class typed values during typeck**, then **lowered post-typeck** into ordinary struct + impl AST. Decouples closure semantics (need typeck info — capture types, body inference) from later passes (which see only ordinary structs and impls).

The chosen design avoids two failed alternatives:
- *Pre-typeck lowering* with generic capture/arg types (`struct __c<T0, A0> { c0: &T0 }`) fails because pocket-rust rejects polymorphic-body operations on unbounded `T` — `|x| x + 1` can't typecheck if `x: A0` is generic.
- *Closures as opaque codegen-only types* would force every later pass to special-case closures, defeating the principle that closures decompose into structs + impls.

### Pipeline shape (per crate)

1. **Parse + derive expand + module resolution** — closures arrive as `ExprKind::Closure` AST nodes.
2. **Initial typeck** — closures stay nodal. Each `check_closure` allocates a synthesized struct path `__closure_<counter>`, type-checks the body under a closure scope (capture barrier on the locals stack), records a `PendingClosure` on `ctx.closure_records`. Bidirectional inference from `Fn(A) -> R` bound at the call site flows expected param/return types into the body.
3. **Post-typeck struct registration** (`register_closure_structs`) — walks each FnSymbol's `closures` vec, registers a `StructEntry` per closure with one field per capture (Copy → T, Ref → `&'cap T`, RefMut → `&'cap mut T`; `'cap` lifetime param when needed).
4. **Closure-lowering pass** (`closure_lower::lower`) — walks each function body, replaces every `ExprKind::Closure` with a struct literal (`__closure_<id> { c0: <init>, c1: <init>, ... }`) and synthesizes `Item::Impl` nodes for each Fn-family flavor. Synth order: FnOnce → FnMut → Fn (so Fn/FnMut signature validation can resolve `Self::Output` via the FnOnce impl's binding).
5. **Setup-delta + typeck-delta** — `register_synthesized_closure_impl` registers each new impl in TraitTable + the methods in FuncTable + runs `check_function` on the synthesized method bodies.
6. **Borrowck / safeck / codegen** — operate on the lowered AST. No closure-specific code paths required.

### Trait selection

The set of impls synthesized per closure is driven by `move` keyword + body mutation analysis:

| closure shape | impls |
| --- | --- |
| `move`-keyword | FnOnce only |
| non-move + body mutates a capture | FnMut + FnOnce |
| non-move + read-only body | Fn + FnMut + FnOnce |

When `body_mutates_capture && !is_move`, FnOnce's body is wrapped in `{ let mut __closure_self = self; <body> }` so the rewrite-to-self-field-access works through the `mut` rebinding (pocket-rust function params are immutable bindings — direct `self.x = …` on the by-value `self` would fail).

### Capture mode + struct field types

Mode is decided at end-of-fn finalize:

- `move` keyword set → `Move` for every capture (regardless of Copy-ness).
- otherwise:
  - mutated + Copy → `Move` (mutation through `&mut self`)
  - mutated + non-Copy → `RefMut` (`&'cap mut T` field)
  - read-only + Copy → `Move` (by-value field, read through autoref)
  - read-only + non-Copy → `Ref` (`&'cap T` field)

Mutation is observed during typeck of the body via:
- `check_assign_stmt` — direct `x = ...` or `x.field = ...` where `x` is a captured root.
- `check_method_call` — when the dispatch picks `recv_adjust = BorrowMut` AND the receiver root is a captured Var (covers compound-assign desugars `x += rhs` → `x.add_assign(rhs)` and explicit `&mut self`-method calls).

The capture is recorded with `mutated: true` even on first observation in the LHS position (the rhs's Var-lookup hasn't run yet at that point in `check_assign_stmt`), so `upgrade_capture_to_ref_mut` ALSO appends a fresh PendingCapture if not yet present.

### Capture barrier + scope tracking

`CheckCtx` carries:
- `closure_scopes: Vec<ClosureScope>` — innermost-last stack of frames. Each frame: `local_barrier: usize` (locals length at scope entry), `node_id` (the closure's NodeId), `synthesized_struct_path`, `captures: Vec<PendingCapture>`.
- `closure_records: Vec<Option<PendingClosure>>` — sized to `func.node_count`, indexed by closure NodeId. Finalized into `ClosureInfo` at end-of-fn.
- `expected_closure_signatures: Vec<Option<(Vec<InferType>, InferType)>>` — bidirectional-inference side channel; `check_call` stashes `Fn(A) -> R` bound's args/output before recursing into a closure arg, `check_closure` consumes on entry.
- `bare_closure_calls: Vec<Option<String>>` — call-site bare-call sugar marker; mono uses to lower `f(args)` Calls as MethodCall MonoExprs.

`PendingCapture { binding_name, captured_ty, mutated: bool }`.

Capture-record helper `record_capture_if_needed(ctx, name, idx)` is the single source of truth for the barrier check — both value-position `Var` (`check_expr_inner`) and place-position `Var` (`check_place_inner`) call it. Without sharing, place-position uses (method-call receiver, `&captured`, deref base, assignment LHS) silently skip capture recording and synthesis sees zero captures.

### Call dispatch

**`f.call((args,))`** — when receiver is a synthesized closure struct, `check_method_call` routes to `check_closure_method_call` (special-case before normal candidate lookup). Looks up the closure's recorded signature in `ctx.closure_records` (current fn) or `funcs.{entries,templates}[*].closures` (cross-fn) by struct path match, type-checks the args tuple against `Tuple(param_types)`, populates `MethodResolution.trait_dispatch` (trait_path = `std::ops::Fn`, trait_args = `[Tuple(P0, ...)]`, recv_type = closure struct). Codegen's `solve_impl_with_args` resolves the impl row at emit time.

**`f(args)`** — `check_call`'s top branch detects a single-segment-path callee. **Locals shadow functions**: when a local with that name exists, route by the local's type (closure → bare-call dispatch via `check_bare_closure_call`, anything else → `expected function, found <ty>` matching rustc E0618). Function-table / variant lookups only fire when no local with that name exists. Without local-first resolution, `let foo: u32 = …; foo(5)` silently calls a fn named `foo` if one is in scope. Same trait_dispatch shape as `.call(...)`, plus records the binding name on `bare_closure_calls[id]` so mono can lower the Call as a MethodCall MonoExpr (closure local as recv, args wrapped in `MonoExprKind::Tuple`).

Recv-adjust per family + receiver shape:
| method | owned recv | `&` recv | `&mut` recv |
| --- | --- | --- | --- |
| `call` | BorrowImm | ByRef | ByRef |
| `call_mut` | BorrowMut | error | ByRef |
| `call_once` | Move | error | error |

### Bidirectional inference

`GenericTemplate.type_param_bound_args: Vec<Vec<Vec<RType>>>` — parallel to `type_param_bounds`, stores positional trait-args at each bound site (resolved via `resolve_trait_ref`). Populated during setup so call-time inference can read the `(P,)` tuple out of an `F: Fn(P) -> R` bound.

Flow: in `check_call`'s template branch, for each closure-typed arg whose corresponding param is `Param("F")`, `lookup_fn_bound_signature` walks F's bounds for any of `std::ops::Fn{,Mut,Once}` and extracts `(args_tuple, Output)`. Stashes (params, return) on `ctx.expected_closure_signatures[closure.id]`. `check_closure` consumes the entry on entry; unannotated params/return adopt the expected types instead of fresh inference vars.

The assoc-constraint check at the call site (verifying `F: Fn<Output = u32>` etc) is **skipped for closure-struct receivers** — the synthesized impl's Output binding doesn't yet exist in `traits.impls` at typeck time. The body-check enforces correctness by construction.

### Side tables on FnSymbol/GenericTemplate

- `closures: Vec<Option<ClosureInfo>>` — finalized closure records, sized to node_count.
- `bare_closure_calls: Vec<Option<String>>` — bare-call binding-name marker, sized to node_count.
- `type_param_bound_args` (templates only) — positional trait-args parallel to `type_param_bounds`.

`ClosureInfo`:
```
pub struct ClosureInfo {
    synthesized_struct_path: Vec<String>,
    param_types: Vec<RType>,
    return_type: RType,
    is_move: bool,
    captures: Vec<CaptureInfo>,
    body_span: Span,
    source_file: String,
    body_mutates_capture: bool,        // drives Fn-skip in synthesis
    enclosing_type_params: Vec<String>, // enclosing fn's type-params; threaded into struct + impl
}

pub struct CaptureInfo { binding_name, captured_ty, mode }
pub enum CaptureMode { Move, Ref, RefMut }
```

### Synthesized impl structure (lowering)

```rust
// Per closure, in synthesis order (FnOnce → FnMut → Fn):
struct __closure_<idx>;                                      // unit struct (no captures)
struct __closure_<idx><'cap> { c0: &'cap T0, c1: T1 }        // mixed mode (cap'd ref + Copy by-value)

// Each impl in the family-set:
impl<'cap> std::ops::FnOnce<(P0, P1, ...)> for __closure_<idx><'cap> {
    type Output = R;
    fn call_once(self, __args: (P0, P1, ...)) -> R { /* body */ }
}
impl<'cap> std::ops::FnMut<(P0, P1, ...)> for __closure_<idx><'cap> {
    fn call_mut(&mut self, __args: (P0, ...)) -> R { /* body */ }
}
impl<'cap> std::ops::Fn<(P0, P1, ...)> for __closure_<idx><'cap> {
    fn call(&self, __args: (P0, ...)) -> R { /* body */ }
}
```

Method body: `let <pat0> = __args.0; let <pat1> = __args.1; ...; <closure body>` — each `<patN>` is the closure's own (irrefutable) param pattern, deep-cloned via `clone_pattern_fresh_ids` so single-binding `|x|` becomes `let x = __args.0;` and tuple destructure `|(a, b)|` becomes `let (a, b) = __args.0;`. The body is a deep-clone of `closure.body` into a fresh NodeId space, with `Var(captured_name)` rewrites:
- `Move` mode → `self.<name>` (FieldAccess on Var("self"))
- `Ref` mode → `*self.<name>` (Deref of FieldAccess)
- `RefMut` mode → `*self.<name>` (same — write through `&mut T` works)

The rewrite is **scope-aware**: `clone_expr_fresh_ids_scoped` threads a `shadowed: &Vec<String>` set through the deep-clone, extending it at every binding-introducing boundary (`let`, match-arm pattern, if-let / for-loop pattern, let-else). When a `Var(name)` is in both `captures` AND `shadowed`, the inner binding wins and the rewrite is skipped. `collect_pattern_bindings` walks a Pattern and accumulates every introduced name (Binding, At, VariantTuple/Struct elems, Tuple, Ref, first arm of Or). Without scope tracking, an inner `let x = ...` shadowing a captured `x` would silently swap in the captured value.

**Nested closures**: `rewrite_expr` recurses into the children-walk's `ExprKind::Closure` arm BEFORE the late consume-and-replace pass at the bottom. This guarantees inner closures get rewritten (their own synth impls pushed to `out`) before the outer's `clone_expr_fresh_ids_scoped` walks the body — the cloner panics on `ExprKind::Closure`, so nested closures must already be replaced with struct-lits when it runs.

For FnOnce when `body_mutates_capture`: receiver name in rewrite is `__closure_self`, body is wrapped in `{ let mut __closure_self = self; <body> }`.

### Synth span uniqueness

All Fn-family impls for the same closure share `info.body_span`. `find_trait_impl_idx_by_span` keys on `(file, start.line, start.col)`. To disambiguate, each impl bumps `start.col` by a per-family offset (Fn=0, FnMut=1, FnOnce=2) so the three rows have distinct identities.

### AST item ordering vs FnSymbol idx

Synthesized impls get registered (FnSymbol idx assigned) during the new_items loop, then appended to `module.items` in the SAME order so codegen's emission order = registration order. Reverse ordering would desync `FnSymbol.idx` from the wasm function index codegen actually assigns — call sites would then dispatch through the wrong wasm idx.

### Generic enclosing fns

`ClosureInfo.enclosing_type_params` carries the enclosing fn's type-params (snapshot of `ctx.type_params` at `check_closure` time). Three downstream consumers thread them through:
- `push_closure_struct` → `StructEntry.type_params` so `__closure_<id>` is generic over the enclosing T.
- `rewrite_expr` (closure expression site) → struct-lit's last path segment carries `args = [Type::Path(T), ...]` so the typeck re-check at the closure expression site binds the type-args.
- `synthesize_impl_for_closure` → `ImplBlock.type_params` and target's last segment's `args` so the synthesized impl is `impl<T> Fn<(T,)> for __closure_<id><T>`.
- `register_synthesized_closure_impl` reads `ib.type_params` (not hardcoded empty) so the impl-method's body resolves `T` against the impl's type-param scope.

Without all four, a closure inside `fn helper<T>(...)` errors `unknown type: T` when the synthesized method's body is re-typed.

### Template body sync after lowering

`GenericTemplate.func` is a clone of the AST taken at typeck setup time; mono reads from this clone, not from `module.items`. After `process_fn` rewrites a function body in-place, `sync_template_body` copies the updated body back into the matching template. Without sync, generic functions containing closures still have unrewritten `ExprKind::Closure` nodes when mono walks them, panicking at the `unreachable!` in `walk_expr`.

## Open work

- **`&mut x` borrow detection**: capture-mode upgrade not yet wired through `&mut Var(captured)` borrow expressions (less common than direct/compound assigns, which are wired).
- **AssocProj cast gap**: `let f = |x| x + 1; f.call((5,)) as u32` (gap-tested) — both sides are unconstrained num-lit Vars; the AssocProj `<?int as Add>::Output` stays unresolved at typeck time, so the cast fails. Fix would propagate the cast's expected type into the closure's return-var.
- **Function items as Fn**: passing `foo` where `Fn(...)` is expected (auto-impl on fn items + fn-pointer types `fn(T) -> R`).
- **`dyn Fn` trait objects**: not yet supported.
- **Return-position `impl Trait`** (RPIT): not implemented; rejected at `resolve_type`.
- **Async closures / generators**: out of scope.
