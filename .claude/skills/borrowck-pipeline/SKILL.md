---
name: borrowck-pipeline
description: Use when modifying or debugging pocket-rust's borrow checker (`src/borrowck/`). Covers the CFG submodule layout, the five-stage pipeline (build → regions → moves → liveness → borrows), region inference + outlives solving, NLL move/borrow semantics, raw-pointer deref behavior, and reborrow patterns the borrow checker recognizes.
---

# borrowck pipeline

Entry: `pub fn check(&Module, &StructTable, &EnumTable, &TraitTable, &mut FuncTable) -> Result<(), Error>` in `src/borrowck/mod.rs`. A thin per-function driver that walks every function body, hands it to the CFG pipeline, runs the outlives solver, then move/liveness/borrow analyses, and writes the resulting `moved_places`/`move_sites` back onto each `FnSymbol`/`GenericTemplate` for codegen's drop-flag synthesis.

Casts to raw pointer types drop borrow tracking (raw pointers carry no compile-time lifetime). Deref-rooted assignments (`*p = …;`, `(*p).f = …;`) succeed against active borrows — typeck's exclusivity invariant on `&mut T` covers them, and `*mut T` is unsafe and out of scope for borrowck.

## Submodule layout (`src/borrowck/`)

- `cfg.rs` — CFG data types: `BasicBlock`, `Terminator`, `CfgStmt`, `Operand` (carrying `node_id: Option<NodeId>` + `span` for error attribution), `Place` + `Projection`, `Place::render(&locals)` for diagnostic strings. Also `RegionId`, `STATIC_REGION` (always `RegionId(0)`), `OutlivesConstraint`, `ConstraintSource`, and `RegionGraph` — see "Region inference" below.
- `build.rs` — AST → CFG lowering with proper control-flow edges for `if` / `match` / `if let` / `while` / `break` / `continue`, including pattern lowering (variant discriminant tests, struct/tuple destructuring, or-patterns, ranges, at-bindings), method-receiver autoref via `synth_borrow`, and **eager-materialization of non-Copy place reads** via `materialize_if_move` so source-order move effects land in the IR before subsequent operands are lowered. Without this, `f(o.x, g(&o))` would record the move of `o.x` only as part of the outer Call statement, after `&o` had already been processed. After `lower_block` populates the CFG, `populate_signature_regions` (run BEFORE the body) and `populate_body_constraints` (run AFTER) build the per-fn `RegionGraph`.
- `regions.rs` — outlives solver. Splits the graph's edges into declared facts (`WhereClause` + `StaticOutlives`) and requirements; computes the Floyd-Warshall transitive closure of the facts and verifies each requirement.
- `moves.rs` — forward dataflow on per-place move state. **Worklist seeded with all blocks** (entry-only seeding caused first-iteration no-change to suppress propagation — a load-bearing fix). Checks reads against the moved set, rejects partial-move-of-Drop with a Drop-specific error, produces `block_in`/`block_out` + `moved_locals` + `move_sites`.
- `liveness.rs` — per-LocalId backward dataflow with a sorted `Vec<LocalId>` LiveSet.
- `borrows.rs` — forward NLL active-borrow tracking. Drops dead borrows using per-statement liveness, validates conflicts (mutable+anything, shared+mutable, write/move-blocked-by-borrow), and propagates borrows through value-carrying rvalues (Use/Cast/Call/StructLit/Tuple/Variant/Builtin): when an rvalue's destination type contains a reference (checked via `rtype_contains_ref` — Ref directly, or any Struct/Enum with non-empty `lifetime_args`/ref-bearing `type_args`, or any Tuple element that contains a ref), every active borrow whose `dest` is one of the rvalue's source locals is duplicated with `dest = stmt.place.root`. This is what keeps `let pt2 = { let pt3 = &pt1; pt3 };` blocking a subsequent `let invalid = pt1;` and what propagates input-arg borrows through ref-returning calls.

## Move/borrow semantics

- Owned Copy primitives (ints, raw pointers) and shared refs (`&T`) don't move on read; owned non-Copy values (structs, `&mut T`) do. The Copy-ness check happens during CFG lowering (`is_copy_with_bounds` in `borrowck/build.rs`'s `move_or_copy`/`materialize_if_move`), so each Operand is correctly tagged as Move or Copy at construction time.
- A move of place `P` rejects subsequent reads of `P` or any place that overlaps it (prefix in either direction). A partial-move of a Drop-typed root is rejected outright (the destructor needs the whole value).
- An assignment to place `P` reinitializes `P` and any sub-places — `borrowck::moves`'s `init` purges descendant entries from the moved set. Borrow conflicts on the same write are caught separately by `borrowck::borrows`'s `check_write`.
- NLL: `borrowck::liveness` runs per-LocalId backward dataflow; `borrowck::borrows` prunes any active borrow whose `dest` is dead at the current statement. So a borrow lives until the borrowing local's last use, not until scope end. This is what lets `let r = &pt; let _v = read(&pt); let m = &mut pt;` work.
- `if`/`match`/`if let` produce branching CFG edges; the move-state at each block's join is the merge (Moved if all preds Moved, MaybeMoved otherwise). `move_sites` accumulates across branches — each entry maps a NodeId to a binding name, and codegen clears the corresponding drop flag at every site that runs.

## Field-access move tracking

Field-access move tracking is field-type-sensitive: a chain like `o.p.a` is a partial move if the chain's tail type is non-Copy, and a value copy (no move recorded — but still rejected if the place has been previously moved) if the tail is Copy. The field type is read from `expr_types[fa.id]`.

Reads of `&T`-typed locals don't record moves (refs are `Copy`), and field-access chains rooted in a `&T` local are likewise treated as non-moving. So `Rect { top_left: d.primary.top_left, bottom_right: d.secondary.bottom_right }` is fine (disjoint paths), `Rect { top_left: d.primary, bottom_right: d.primary.top_left }` errors at the second use.

## Concretely

- `f(&p, p.y)` rejects: arg 0's `&p` registers an active borrow on `p`. Then arg 1's read of `p.y` (or `f`'s second-arg position trying to move `p.y`) conflicts with the still-active borrow.
- `Pair { first: x_of(&p), second: p.y }` is accepted: `x_of(&p)` materializes a temp local holding the call result; the temp is non-ref (returns `usize`), so propagation doesn't carry forward the `&p` borrow. After `x_of` returns, no live local holds the borrow, and `p.y` is fine.
- `let pt2 = { let pt3 = &pt1; pt3 }` keeps the borrow alive past the inner block: `pt3 = &pt1` registers `{dest: pt3, place: pt1}`. The inner block's tail is `pt3`, which the outer let assigns to `pt2`. Borrow propagation in `borrowck::borrows` (rvalue dest is `&u32`, contains a ref) duplicates the borrow with `dest: pt2`. A subsequent `let invalid = pt1;` correctly rejects.
- `let v = { let r = &pt1; r.x }` accepts a subsequent `let q = pt1;`: `v` is a `usize` copy, doesn't carry a borrow. After the inner block ends, `r` is dead, NLL pruning drops the `&pt1` borrow.

## Reborrow exceptions to "non-Copy moves on read"

**Implicit method-receiver reborrow:** when a method-call receiver is itself a ref (`&T` or `&mut T`) and the method takes a ref-typed self (recv_adjust = ByRef), `borrowck/build.rs::lower_recv_reborrow` lowers it as `Operand::Copy(place)` instead of the default Move. Justified semantically because the callee scope-bounds the borrow — after the call, the source binding can resume use. Without this, `&mut self` methods couldn't transitively call other `&mut self` methods on the same binding (the receiver would be Move-consumed by the inner call).

**Implicit function/builtin-arg reborrow for `&mut T`:** `materialize_if_move` treats any `&mut T` argument as Copy (same as the method-receiver case). Same justification — the call scope-bounds the borrow. Without this, calling two builtins or functions with the same `&mut self` arg in a method body (e.g. `IndexMut for str`'s body using `¤str_len(self)` then `¤str_as_mut_bytes(self)`) errors with "self already moved". Sound because the reborrow is exclusive for the duration of the call, just like Rust's auto-reborrow.

**Implicit deref reborrow:** when `*expr` is lowered to a place via `lower_expr_place`'s `Deref` arm, the inner ref expression is read as a place (not consumed) when it's a place-form expression (`Var` / `FieldAccess` / `TupleIndex` / nested `Deref`). This is what lets `*self = ¤u8_add(*self, other);` typecheck even though `&mut T` is non-Copy — both `*self` reads use `self` without recording a move. Non-place inners (`*foo()`) still go through the materialization path, which is appropriate because the temp holds the only copy of the ref.

**Raw-pointer deref doesn't track moves:** when an `Operand::Move(place)` has a `Deref` projection rooted at a `*const T` / `*mut T` local, `apply_operand` skips `state.mark(place, Moved)` (`is_through_raw_ptr_deref`). Raw pointers don't carry compile-time ownership, so `let v = unsafe { *raw };` followed by `raw.cast::<U>()` is a sound reading-from-pointer pattern that the place-overlap check would otherwise reject (the deref-move on `*raw` makes `raw` itself appear "already moved" via the prefix overlap). `Box::into_inner` and similar raw-ptr-extraction patterns rely on this.

## Region inference

Per-fn `RegionGraph` (`src/borrowck/cfg.rs`) tracks lifetime relationships. Built in two phases by `borrowck/build.rs` and consumed by `borrowck/regions.rs`.

**Region kinds.** RegionId 0 is `'static`. Sig-fixed regions are populated at L1's `populate_signature_regions`: each `<'a>` lifetime param gets a slot in `sig_named`; each `LifetimeRepr::Inferred(N)` in param/return types gets a slot in `sig_inferred`. Body-fresh regions are everything else — allocated by `fresh_region()` for borrows and for body let-bindings whose ref lifetime is `Inferred(0)`.

**Constraint sources** (`enum ConstraintSource`):
- *Declared:* `WhereClause` (from `where 'a: 'b`), `StaticOutlives` (seeded `'static : <every other r>` at graph build).
- *Required:* `FnReturn` (returned ref's region vs `fn_return_region`), `CallArg` (caller's arg vs callee's instantiated param region), `CallReturn` (callee's instantiated return region vs caller's destination), `Reborrow` (source ref's region vs reborrow's region), `Assign` (rhs vs lhs at any same-typed binding boundary).

**Solver semantics** (`regions::solve`). Floyd-Warshall closure of declared facts. For each required edge `sup → sub`:
- If both endpoints are sig-fixed: required edge must be in the closure; otherwise error with the constraint's span and (when both endpoints are `sig_named`) a `consider adding 'a: 'b` suggestion.
- If either endpoint is body-fresh: skip — the solver picks any value for the body-fresh region that satisfies. (This is the "loose" choice; real Rust does proper region-variable inference. Loose-but-correct: won't reject valid programs, but doesn't catch every invalid one. rt5 #8's call-site outlives violation needs scope-bound modeling on body regions to catch — deferred.)

**Variance** (`src/typeck/variance.rs`). Each struct/enum carries `type_param_variance` and `lifetime_param_variance` parallel to `type_params` / `lifetime_params`. Computed by use-site analysis: walk fields, narrow each (struct, slot) variance from `Covariant` (default) to `Invariant` on detection of an invariant use. Lattice is a chain (no `Contravariant` until fn pointers land); fixpoint converges in O(structs × params) flips. Position-flipping rules:
- `T` direct: inherits position.
- `&T`, `&'_ T` outer lifetime: covariant; inner T inherits.
- `&mut T`, `&'_ mut T`: outer lifetime covariant; inner T flips to **Invariant**.
- `*const T` / `*mut T`: T flips to Invariant.
- `Struct<T_i>`: composes Struct's own variance for slot i with the current position.
- Tuple/Slice elements: inherit position.
- `AssocProj { base, ... }`: base flips to Invariant.

The variance vectors are read by L3's body constraint emitter when emitting outlives between two same-path types with differing region/type args (Covariant slot → one-way edge; Invariant slot → equate via two edges).

**Build-time invariants:**
- `region_count` starts at 1 (RegionId 0 reserved for `'static`); `fresh_region()` returns 1, 2, ….
- `sig_named` and `sig_inferred` are frozen after L1's `populate_signature_regions`. L3's `resolve_or_alloc_region` only LOOKS UP — it never pushes. The solver uses membership in those vectors to decide fixed vs body-fresh.
- Lifetime in-scope check goes through `crate::typeck::lifetime_in_scope(name, &fn_lifetime_params)` — single source of truth that recognizes user params plus built-ins (`'static` today). Don't sprinkle ad-hoc checks; route through this predicate.

## Output to codegen

Borrowck writes back per-`FnSymbol`/`GenericTemplate`:
- `moved_places: Vec<MovedPlace>` — per-place final move status (`Moved` or `MaybeMoved`).
- `move_sites: Vec<(NodeId, name)>` — every whole-binding move site in the function. Codegen clears drop flags at these sites for `MaybeMoved` bindings.
