// AST → CFG converter. Lowers a typeck'd `Function` body into a `Cfg`.
//
// Each compound expression evaluates left-to-right; intermediate values
// land in compiler-introduced temporary `LocalDecl`s. Control-flow
// expressions (if/match/if-let, future while) split blocks and merge at
// a successor.

use crate::ast::{
    self, AssignStmt, Block, Call, Expr, ExprKind, Function, IfLetExpr, LetStmt,
    MatchExpr, MethodCall, Pattern, PatternKind, Stmt, StructLit,
};
use super::cfg::{
    BasicBlock, BlockId, CallTarget, Cfg, CfgStmt, CfgStmtKind, LocalDecl, LocalId, Operand,
    OperandKind, Place, Projection, RegionId, Rvalue, Terminator, VariantFields,
};
use crate::span::Span;
use crate::typeck::{
    CallResolution, EnumTable, EnumVariantEntry, FuncTable, IntKind, LifetimeRepr,
    MethodResolution, RType, ReceiverAdjust, StructTable, TraitTable, VariantPayloadResolved,
    is_copy_with_bounds, substitute_rtype,
};

// Per-binding-name → LocalId map; lookup walks the stack of scopes from
// innermost out.
struct Scope {
    names: Vec<(String, LocalId)>,
}

pub struct CfgBuildCtx<'a> {
    pub structs: &'a StructTable,
    pub enums: &'a EnumTable,
    pub traits: &'a TraitTable,
    pub funcs: &'a FuncTable,
    pub expr_types: &'a Vec<Option<RType>>,
    pub method_resolutions: &'a Vec<Option<MethodResolution>>,
    pub call_resolutions: &'a Vec<Option<CallResolution>>,
    // Per-NodeId binding name for bare-closure / bare-typeparam calls.
    // When set, `f(args)` is dispatched as `f.<call|call_mut|call_once>(args)`
    // (typeck records this in `check_bare_closure_call` /
    // `check_bare_typeparam_fn_call`). Borrowck consults it to record the
    // synthesized receiver effect — Move recv_adjust → move-out on the
    // binding — even though the surface AST node is `Call`, not `MethodCall`.
    pub bare_closure_calls: &'a Vec<Option<String>>,
    // Per-NodeId const-use slots (see `FnSymbol.const_uses`). When a
    // `Var` resolves to a const item rather than a local, the slot is
    // `Some(value)`. Borrowck lowers such Vars to inline constant
    // operands rather than place reads — consts have no place.
    pub const_uses: &'a Vec<Option<crate::typeck::ConstValue>>,
    // Per-NodeId fn-item address (see `FnSymbol.fn_item_addrs`). When
    // a `Var(name)` resolves to a fn item being coerced into an
    // `RType::FnPtr` slot, the entry is `Some(callee_idx)`. Borrowck
    // treats the value as a Copy scalar (the funcref slot index) with
    // no underlying place to track — emitted as a placeholder constant
    // operand.
    pub fn_item_addrs: &'a Vec<Option<usize>>,
    // User-declared lifetime parameter names on the fn (for `<'a, 'b>`
    // — empty if the fn has no lifetime generics). Used by Phase L1 to
    // populate `RegionGraph.sig_named`.
    pub lifetime_params: &'a Vec<String>,
    // Resolved `where 'a : 'b1 + 'b2 + …` clauses on the fn. Read by
    // Phase L1 to seed the outlives graph with WhereClause-source
    // edges. Each predicate's lhs/bounds reference names from
    // `lifetime_params` (or `'static`).
    pub lifetime_predicates: &'a Vec<crate::typeck::LifetimePredResolved>,
    pub type_params: &'a Vec<String>,
    pub type_param_bounds: &'a Vec<Vec<Vec<String>>>,
    // Resolved parameter types (in order). Length = func.params.len().
    pub param_types: &'a Vec<RType>,
    // Resolved return type (`()` if absent in source).
    pub return_type: &'a RType,
    // Per-pattern.id ergonomics record from typeck. Borrowck reads
    // this at every pattern-test site to peel ref layers off the
    // scrutinee before dispatching the pattern's kind-specific test —
    // so a `Some(x)` pattern matched against a `&Option<T>` scrutinee
    // walks through a `Deref` projection first.
    pub pattern_ergo: &'a Vec<crate::typeck::PatternErgo>,
}

// Active-loop bookkeeping for break/continue.
struct LoopFrame {
    label: Option<String>,
    // Block to jump to when continuing the loop (the cond-eval block).
    continue_target: BlockId,
    // Block to jump to when breaking out of the loop.
    break_target: BlockId,
}

struct Builder<'a> {
    ctx: &'a CfgBuildCtx<'a>,
    blocks: Vec<BasicBlock>,
    locals: Vec<LocalDecl>,
    // Allocator counter for `fresh_region`. Starts at 1; RegionId 0
    // is reserved for `'static` (`STATIC_REGION` in cfg.rs).
    region_count: u32,
    region_graph: crate::borrowck::cfg::RegionGraph,
    // Per-LocalId outermost region: the RegionId for the binding's
    // outer lifetime when its type is `&'r T` (or a struct/enum whose
    // first slot is a region we care about). `None` for non-ref
    // bindings — `Use`/`Move`/`Copy` of an owned local emits no region
    // constraint. Populated lazily by `binding_region_lookup` so locals
    // allocated mid-CFG-walk get an entry by the time the constraint
    // pass needs it.
    binding_region: Vec<Option<crate::borrowck::cfg::RegionId>>,
    current_block: BlockId,
    scopes: Vec<Scope>,
    return_local: Option<LocalId>,
    param_count: u32,
    loops: Vec<LoopFrame>,
}

pub fn build(func: &Function, ctx: &CfgBuildCtx) -> Cfg {
    let mut b = Builder {
        ctx,
        blocks: Vec::new(),
        locals: Vec::new(),
        // Reserve RegionId 0 for `'static` (see `STATIC_REGION` in
        // cfg.rs). Subsequent `fresh_region()` calls return 1, 2, …
        region_count: 1,
        region_graph: crate::borrowck::cfg::RegionGraph::new(),
        binding_region: Vec::new(),
        current_block: 0,
        scopes: Vec::new(),
        return_local: None,
        param_count: 0,
        loops: Vec::new(),
    };
    // Phase L1: signature walk. Allocate RegionIds for each named
    // lifetime + each elided-ref `Inferred(N)` slot, seed
    // `'static : <every-other-region>`, and emit WhereClause edges
    // for `where 'a : 'b` predicates. Behaviorally a no-op until L3
    // adds body constraints and L4 runs the solver.
    populate_signature_regions(&mut b, func);
    let entry = b.new_block();
    b.current_block = entry;

    // Reserve return local 0 if non-unit return.
    if !is_unit(ctx.return_type) {
        let id = b.alloc_local(
            None,
            ctx.return_type.clone(),
            func.name_span.copy(),
            true,
            false,
        );
        b.return_local = Some(id);
    }

    b.push_scope();
    // Allocate parameter locals.
    let mut i = 0;
    while i < func.params.len() {
        let p = &func.params[i];
        let rt = ctx.param_types[i].clone();
        let id = b.alloc_local(Some(p.name.clone()), rt, p.name_span.copy(), p.mutable, false);
        b.bind_name(&p.name, id);
        b.param_count += 1;
        i += 1;
    }

    // Lower the body. The function's tail expression (if any) feeds the
    // return local; otherwise the body is unit-returning and we just
    // run statements.
    b.lower_block(&func.body, b.return_local.map(local_place));
    // Final terminator.
    b.set_terminator(Terminator::Return);
    b.pop_scope();

    // Phase L3: body region inference. Walk the populated CFG and
    // emit outlives constraints for each region-relevant op:
    // assignments, returns, calls. Constraints land on
    // `b.region_graph.outlives` and are validated by Phase L4.
    populate_body_constraints(&mut b);

    Cfg {
        blocks: b.blocks,
        locals: b.locals,
        entry,
        region_count: b.region_count,
        region_graph: b.region_graph,
        return_local: b.return_local,
        param_count: b.param_count,
    }
}

// Walk the fn signature: assign a RegionId to each named lifetime
// (`<'a, 'b>`) and to each `LifetimeRepr::Inferred(N)` (elided refs).
// Seed `'static : <every other region>` (the StaticOutlives source).
// Translate `where 'a : 'b1 + 'b2` into WhereClause-source edges.
// Record the outermost return-region if the return type is a ref —
// body returns will later emit `value : fn_return_region`.
fn populate_signature_regions(b: &mut Builder, func: &Function) {
    use crate::borrowck::cfg::{ConstraintSource, OutlivesConstraint, STATIC_REGION};
    use crate::typeck::LifetimeRepr;

    // 1. Allocate region IDs for each user-declared lifetime param.
    let mut i = 0;
    while i < b.ctx.lifetime_params.len() {
        let r = b.fresh_region();
        b.region_graph
            .sig_named
            .push((b.ctx.lifetime_params[i].clone(), r));
        i += 1;
    }

    // 2. Walk param + return types collecting Inferred lifetimes (one
    // RegionId per distinct `Inferred(N)` id) and any Named lifetimes
    // not declared by the fn (these would have been rejected upstream
    // by `validate_named_lifetimes`, but we accept `'static` and any
    // re-encountered name no-op).
    let mut i = 0;
    while i < b.ctx.param_types.len() {
        collect_sig_regions(&b.ctx.param_types[i], &mut b.region_graph, &mut b.region_count);
        i += 1;
    }
    collect_sig_regions(b.ctx.return_type, &mut b.region_graph, &mut b.region_count);

    // 3. Outermost region of the return type: if it's a ref, record
    // it for body-return constraints.
    if let RType::Ref { lifetime, .. } = b.ctx.return_type {
        let r = lookup_or_static(&b.region_graph, lifetime);
        b.region_graph.fn_return_region = r;
    }

    // 4. `'static` outlives every other region. Seed each pair with a
    // StaticOutlives edge so the solver doesn't need to special-case
    // RegionId 0 — it falls out of the closure.
    let max = b.region_count;
    let mut r = 1u32;
    while r < max {
        b.region_graph.outlives.push(OutlivesConstraint {
            sup: STATIC_REGION,
            sub: r,
            span: func.name_span.copy(),
            source: ConstraintSource::StaticOutlives,
        });
        r += 1;
    }

    // 5. Where-clauses: `where 'lhs : 'b1 + 'b2 + …` becomes one edge
    // per `'bi`. Resolve each side via `lookup_or_static`. Predicates
    // with unresolvable names are skipped silently — typeck rejects
    // those at signature setup, so unresolved here means the fn
    // wouldn't have type-checked.
    let mut i = 0;
    while i < b.ctx.lifetime_predicates.len() {
        let pred = &b.ctx.lifetime_predicates[i];
        let sup = b.region_graph.lookup_named(&pred.lhs).or_else(|| {
            if pred.lhs == "static" {
                Some(STATIC_REGION)
            } else {
                None
            }
        });
        if let Some(sup) = sup {
            let mut k = 0;
            while k < pred.bounds.len() {
                let sub_name = &pred.bounds[k];
                let sub = b.region_graph.lookup_named(sub_name).or_else(|| {
                    if sub_name == "static" {
                        Some(STATIC_REGION)
                    } else {
                        None
                    }
                });
                if let Some(sub) = sub {
                    b.region_graph.outlives.push(OutlivesConstraint {
                        sup,
                        sub,
                        span: pred.span.copy(),
                        source: ConstraintSource::WhereClause,
                    });
                }
                k += 1;
            }
        }
        i += 1;
    }
}

// Recurse through an RType, mapping each `LifetimeRepr::Inferred(N)`
// to a fresh RegionId (deduped by N) and adding it to
// `sig_inferred`. Named lifetimes are looked up in `sig_named`; if
// they were already populated by the lifetime-params walk, they're
// skipped here.
fn collect_sig_regions(rt: &RType, graph: &mut crate::borrowck::cfg::RegionGraph, counter: &mut u32) {
    use crate::typeck::LifetimeRepr;
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            ensure_lifetime_region(lifetime, graph, counter);
            collect_sig_regions(inner, graph, counter);
        }
        RType::RawPtr { inner, .. } => collect_sig_regions(inner, graph, counter),
        RType::Struct { type_args, lifetime_args, .. }
        | RType::Enum { type_args, lifetime_args, .. } => {
            for la in lifetime_args {
                ensure_lifetime_region(la, graph, counter);
            }
            for ta in type_args {
                collect_sig_regions(ta, graph, counter);
            }
        }
        RType::Tuple(elems) => {
            for e in elems {
                collect_sig_regions(e, graph, counter);
            }
        }
        RType::Slice(inner) => collect_sig_regions(inner, graph, counter),
        RType::AssocProj { base, .. } => collect_sig_regions(base, graph, counter),
        RType::Bool
        | RType::Int(_)
        | RType::Char
        | RType::Str
        | RType::Never
        | RType::Param(_)
        | RType::Opaque { .. } => {}
        // FnPtr inner types may carry refs/lifetime args. Recurse so
        // any inferred lifetimes inside get a region id.
        RType::FnPtr { params, ret } => {
            for p in params {
                collect_sig_regions(p, graph, counter);
            }
            collect_sig_regions(ret, graph, counter);
        }
        RType::Dyn { lifetime, .. } => ensure_lifetime_region(lifetime, graph, counter),
    }
}

fn ensure_lifetime_region(
    lt: &crate::typeck::LifetimeRepr,
    graph: &mut crate::borrowck::cfg::RegionGraph,
    counter: &mut u32,
) {
    use crate::borrowck::cfg::STATIC_REGION;
    use crate::typeck::LifetimeRepr;
    match lt {
        LifetimeRepr::Named(name) => {
            if name == "static" {
                return; // STATIC_REGION already exists.
            }
            // Already populated by lifetime-params walk; skip.
            if graph.lookup_named(name).is_some() {
                return;
            }
            // Name not declared — typeck rejects this at sig setup.
            // Defensively allocate a fresh region so downstream
            // doesn't trip on a missing entry; it'll never be used in
            // a sound program.
            let r = *counter;
            *counter += 1;
            graph.sig_named.push((name.clone(), r));
        }
        LifetimeRepr::Inferred(id) => {
            if graph.lookup_inferred(*id).is_some() {
                return;
            }
            let r = *counter;
            *counter += 1;
            graph.sig_inferred.push((*id, r));
        }
    }
}

fn lookup_or_static(
    graph: &crate::borrowck::cfg::RegionGraph,
    lt: &crate::typeck::LifetimeRepr,
) -> Option<crate::borrowck::cfg::RegionId> {
    use crate::borrowck::cfg::STATIC_REGION;
    use crate::typeck::LifetimeRepr;
    match lt {
        LifetimeRepr::Named(name) => {
            if name == "static" {
                Some(STATIC_REGION)
            } else {
                graph.lookup_named(name)
            }
        }
        LifetimeRepr::Inferred(id) => graph.lookup_inferred(*id),
    }
}

// Resolve a `LifetimeRepr` to a RegionId for a body binding.
//
// Sig-fixed regions (named lifetimes declared on the fn, sig-elided
// `Inferred(N)` slots already allocated by L1's
// `populate_signature_regions`) are looked up. Body-introduced
// lifetimes (`Inferred(0)` placeholders, or names we've never seen —
// defensively, since typeck rejects undeclared names at the sig site)
// get a body-fresh RegionId that the solver treats as a free
// variable.
//
// IMPORTANT: this function does NOT push to `sig_named` /
// `sig_inferred`. Those vectors stay frozen at L1's signature walk;
// the solver uses membership in them to decide whether a region is
// fixed (caller picks the value, body must satisfy) or free (body
// picks the value, satisfies trivially).
fn resolve_or_alloc_region(
    graph: &crate::borrowck::cfg::RegionGraph,
    counter: &mut u32,
    lt: &crate::typeck::LifetimeRepr,
) -> crate::borrowck::cfg::RegionId {
    use crate::borrowck::cfg::STATIC_REGION;
    use crate::typeck::LifetimeRepr;
    match lt {
        LifetimeRepr::Named(name) => {
            if name == "static" {
                return STATIC_REGION;
            }
            if let Some(r) = graph.lookup_named(name) {
                return r;
            }
            let r = *counter;
            *counter += 1;
            r
        }
        LifetimeRepr::Inferred(id) => {
            if *id != 0 {
                if let Some(r) = graph.lookup_inferred(*id) {
                    return r;
                }
            }
            let r = *counter;
            *counter += 1;
            r
        }
    }
}

// Compute the resolved type of a place — root local's type with
// `Field`/`TupleIndex`/`Deref` projections applied. Returns None when
// a projection can't be resolved (e.g. field name not found on the
// projected type).
fn place_type(b: &Builder, place: &crate::borrowck::cfg::Place) -> Option<RType> {
    use crate::borrowck::cfg::Projection;
    let local = &b.locals[place.root as usize];
    let mut ty = local.ty.clone();
    for proj in &place.projections {
        match proj {
            Projection::Field(name) => {
                ty = field_type(b, &ty, name)?;
            }
            Projection::TupleIndex(idx) => match ty {
                RType::Tuple(elems) => {
                    if (*idx as usize) < elems.len() {
                        ty = elems[*idx as usize].clone();
                    } else {
                        return None;
                    }
                }
                _ => return None,
            },
            Projection::Deref => match ty {
                RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => {
                    ty = (*inner).clone();
                }
                _ => return None,
            },
        }
    }
    Some(ty)
}

// Outermost region of a place's type. Convenience wrapper around
// `place_type`. None when the place's type isn't a ref.
fn place_outer_region(
    b: &Builder,
    place: &crate::borrowck::cfg::Place,
) -> Option<crate::borrowck::cfg::RegionId> {
    match place_type(b, place)? {
        RType::Ref { lifetime, .. } => lookup_or_static(&b.region_graph, &lifetime),
        _ => None,
    }
}

// Variance-aware constraint emission for value flow (rt6#3): walk
// paired source/destination types, and at each region-bearing
// position emit outlives edges per the slot's variance composed with
// the current outer position. Covariant slot → one edge `src : dst`.
// Invariant slot → two edges (equate).
//
// The two `resolve` closures translate `LifetimeRepr`s into RegionIds
// in their respective contexts — same closure for both sides at an
// assignment (caller's RegionGraph), different closures at a call
// site (caller graph for src, callee-instantiation map for dst).
//
// Composition rules from `src/typeck/variance.rs`: a struct's
// declared variance for slot i composes with the current outer
// position. `Cov ∘ Cov = Cov`; `Cov ∘ Inv = Inv`; `Inv ∘ _ = Inv`.
// `&mut T`'s inner T is Inv, raw-ptr's pointee is Inv,
// `AssocProj`'s base is Inv. References' OUTER lifetime is always
// covariant (mutability affects only the inner T).
//
// Note: the `&dyn Fn(...)` parameters here are pocket-rust's natural
// shape for "two different lookup strategies." Pocket-rust's parser
// does not yet accept `dyn` (which is why selfhost trips on this
// signature); proper `dyn` support is queued behind the `format!` /
// `Display` / `Write` work, which all need it.
fn emit_subtype_flow(
    src_ty: &RType,
    dst_ty: &RType,
    position: crate::typeck::Variance,
    src_resolve: &dyn Fn(&crate::typeck::LifetimeRepr) -> Option<crate::borrowck::cfg::RegionId>,
    dst_resolve: &dyn Fn(&crate::typeck::LifetimeRepr) -> Option<crate::borrowck::cfg::RegionId>,
    structs: &crate::typeck::StructTable,
    enums: &crate::typeck::EnumTable,
    out: &mut Vec<crate::borrowck::cfg::OutlivesConstraint>,
    span: &crate::span::Span,
    source: crate::borrowck::cfg::ConstraintSource,
) {
    use crate::borrowck::cfg::{ConstraintSource, OutlivesConstraint};
    use crate::typeck::variance::{compose, Variance};
    match (src_ty, dst_ty) {
        (
            RType::Ref { inner: si, lifetime: sl, mutable: sm },
            RType::Ref { inner: di, lifetime: dl, mutable: dm },
        ) => {
            // Reference outer lifetimes: always covariant in the
            // lifetime (regardless of mutability).
            if let (Some(sr), Some(dr)) = (src_resolve(sl), dst_resolve(dl)) {
                push_edge(out, sr, dr, position, span, source);
            }
            // Inner T: covariant for `&T`, invariant for `&mut T`.
            let inner_pos = if *sm || *dm {
                compose(position, Variance::Invariant)
            } else {
                position
            };
            emit_subtype_flow(
                si, di, inner_pos, src_resolve, dst_resolve, structs, enums, out, span, source,
            );
        }
        (
            RType::Struct {
                path: sp,
                type_args: sta,
                lifetime_args: sla,
            },
            RType::Struct {
                path: dp,
                type_args: dta,
                lifetime_args: dla,
            },
        ) if sp == dp => {
            let entry = match crate::typeck::struct_lookup(structs, sp) {
                Some(e) => e,
                None => return,
            };
            let mut i = 0;
            while i < sla.len() && i < dla.len() {
                let slot_var = entry
                    .lifetime_param_variance
                    .get(i)
                    .copied()
                    .unwrap_or(Variance::Invariant);
                let composed = compose(position, slot_var);
                if let (Some(sr), Some(dr)) = (src_resolve(&sla[i]), dst_resolve(&dla[i])) {
                    push_edge(out, sr, dr, composed, span, source);
                }
                i += 1;
            }
            let mut i = 0;
            while i < sta.len() && i < dta.len() {
                let slot_var = entry
                    .type_param_variance
                    .get(i)
                    .copied()
                    .unwrap_or(Variance::Invariant);
                let composed = compose(position, slot_var);
                emit_subtype_flow(
                    &sta[i], &dta[i], composed, src_resolve, dst_resolve, structs, enums, out,
                    span, source,
                );
                i += 1;
            }
        }
        (
            RType::Enum {
                path: sp,
                type_args: sta,
                lifetime_args: sla,
            },
            RType::Enum {
                path: dp,
                type_args: dta,
                lifetime_args: dla,
            },
        ) if sp == dp => {
            let entry = match crate::typeck::enum_lookup(enums, sp) {
                Some(e) => e,
                None => return,
            };
            let mut i = 0;
            while i < sla.len() && i < dla.len() {
                let slot_var = entry
                    .lifetime_param_variance
                    .get(i)
                    .copied()
                    .unwrap_or(Variance::Invariant);
                let composed = compose(position, slot_var);
                if let (Some(sr), Some(dr)) = (src_resolve(&sla[i]), dst_resolve(&dla[i])) {
                    push_edge(out, sr, dr, composed, span, source);
                }
                i += 1;
            }
            let mut i = 0;
            while i < sta.len() && i < dta.len() {
                let slot_var = entry
                    .type_param_variance
                    .get(i)
                    .copied()
                    .unwrap_or(Variance::Invariant);
                let composed = compose(position, slot_var);
                emit_subtype_flow(
                    &sta[i], &dta[i], composed, src_resolve, dst_resolve, structs, enums, out,
                    span, source,
                );
                i += 1;
            }
        }
        (RType::Tuple(se), RType::Tuple(de)) if se.len() == de.len() => {
            let mut i = 0;
            while i < se.len() {
                emit_subtype_flow(
                    &se[i], &de[i], position, src_resolve, dst_resolve, structs, enums, out,
                    span, source,
                );
                i += 1;
            }
        }
        (RType::Slice(si), RType::Slice(di)) => {
            emit_subtype_flow(
                si, di, position, src_resolve, dst_resolve, structs, enums, out, span, source,
            );
        }
        (RType::RawPtr { inner: si, .. }, RType::RawPtr { inner: di, .. }) => {
            // Raw-ptr pointee invariant.
            emit_subtype_flow(
                si,
                di,
                compose(position, Variance::Invariant),
                src_resolve,
                dst_resolve,
                structs,
                enums,
                out,
                span,
                source,
            );
        }
        // Leaves and mismatched-shape pairs (typeck would have
        // rejected the latter): no constraint to emit.
        _ => {
            let _ = (out, span, source);
        }
    }
}

fn push_edge(
    out: &mut Vec<crate::borrowck::cfg::OutlivesConstraint>,
    src: crate::borrowck::cfg::RegionId,
    dst: crate::borrowck::cfg::RegionId,
    position: crate::typeck::Variance,
    span: &crate::span::Span,
    source: crate::borrowck::cfg::ConstraintSource,
) {
    use crate::borrowck::cfg::OutlivesConstraint;
    use crate::typeck::Variance;
    match position {
        Variance::Covariant => {
            out.push(OutlivesConstraint {
                sup: src,
                sub: dst,
                span: span.copy(),
                source,
            });
        }
        Variance::Invariant => {
            out.push(OutlivesConstraint {
                sup: src,
                sub: dst,
                span: span.copy(),
                source,
            });
            out.push(OutlivesConstraint {
                sup: dst,
                sub: src,
                span: span.copy(),
                source,
            });
        }
    }
}

fn field_type(b: &Builder, ty: &RType, name: &str) -> Option<RType> {
    match ty {
        RType::Struct { path, type_args, .. } => {
            let entry = crate::typeck::struct_lookup(b.ctx.structs, path)?;
            // Build env from struct's params → call-site type_args.
            let mut env: Vec<(String, RType)> = Vec::new();
            let mut i = 0;
            while i < entry.type_params.len() && i < type_args.len() {
                env.push((entry.type_params[i].clone(), type_args[i].clone()));
                i += 1;
            }
            for f in &entry.fields {
                if f.name == name {
                    return Some(crate::typeck::substitute_rtype(&f.ty, &env));
                }
            }
            None
        }
        _ => None,
    }
}

// Phase L3 entry point. Walks the completed CFG and emits outlives
// constraints from each region-relevant operation:
//   * Assigns of ref-typed values: rhs_region : lhs_region.
//   * Returns (Assign-to-return-local): value_region : fn_return_region.
//   * Calls: instantiate the callee's free regions; per-arg
//     covariance-aware constraints; where-clause edges; return.
//
// The pass mutates `b.region_graph.outlives` and `b.binding_region`.
// `b.binding_region` is initialized lazily by `binding_region_for`.
fn populate_body_constraints(b: &mut Builder) {
    use crate::borrowck::cfg::{CfgStmtKind, ConstraintSource, OutlivesConstraint, Rvalue};
    // Initialize `binding_region` for every local: ref-typed gets a
    // RegionId via `resolve_or_alloc_region`; others get `None`.
    let n = b.locals.len();
    b.binding_region = vec![None; n];
    let mut i = 0;
    while i < n {
        let ty = b.locals[i].ty.clone();
        if let RType::Ref { lifetime, .. } = &ty {
            let r = resolve_or_alloc_region(&b.region_graph, &mut b.region_count, lifetime);
            b.binding_region[i] = Some(r);
        }
        i += 1;
    }

    // Emit constraints from each block's stmts.
    let block_count = b.blocks.len();
    let mut bi = 0;
    while bi < block_count {
        let stmt_count = b.blocks[bi].stmts.len();
        let mut si = 0;
        while si < stmt_count {
            // Clone stmt to avoid borrow conflict with `b` mutations.
            let kind = clone_stmt_kind(&b.blocks[bi].stmts[si].kind);
            let span = b.blocks[bi].stmts[si].span.copy();
            if let CfgStmtKind::Assign { place, rvalue } = kind {
                emit_assign_constraints(b, &place, &rvalue, &span);
            }
            si += 1;
        }
        bi += 1;
    }
}

fn clone_stmt_kind(k: &crate::borrowck::cfg::CfgStmtKind) -> crate::borrowck::cfg::CfgStmtKind {
    use crate::borrowck::cfg::CfgStmtKind;
    match k {
        CfgStmtKind::Assign { place, rvalue } => CfgStmtKind::Assign {
            place: place.clone(),
            rvalue: rvalue.clone(),
        },
        CfgStmtKind::Drop(p) => CfgStmtKind::Drop(p.clone()),
        CfgStmtKind::StorageLive(l) => CfgStmtKind::StorageLive(*l),
        CfgStmtKind::StorageDead(l) => CfgStmtKind::StorageDead(*l),
        CfgStmtKind::Uninit(l) => CfgStmtKind::Uninit(*l),
    }
}

fn emit_assign_constraints(
    b: &mut Builder,
    lhs_place: &crate::borrowck::cfg::Place,
    rvalue: &crate::borrowck::cfg::Rvalue,
    span: &crate::span::Span,
) {
    use crate::borrowck::cfg::{ConstraintSource, OperandKind, OutlivesConstraint, Rvalue};
    use crate::typeck::Variance;
    // Determine the LHS region: outer region of the assigned-to place.
    // None for non-ref LHS.
    let lhs_region = place_outer_region(b, lhs_place);
    // Identify "this is the function return" — assigns to the
    // return_local with no projections.
    let is_return = b.return_local == Some(lhs_place.root) && lhs_place.projections.is_empty();
    match rvalue {
        Rvalue::Use(operand) => {
            // Variance-aware paired-types walk: emit constraints at
            // every region-bearing position in the operand's type
            // matched to the LHS's expected type. Both sides resolve
            // through the caller's RegionGraph (assign is intra-fn).
            let src_ty_opt = operand_type(b, operand);
            let dst_ty_opt = if is_return {
                Some(b.ctx.return_type.clone())
            } else {
                place_type(b, lhs_place)
            };
            if let (Some(src_ty), Some(dst_ty)) = (src_ty_opt, dst_ty_opt) {
                let mut new_edges: Vec<OutlivesConstraint> = Vec::new();
                // Both sides resolve through the caller's RegionGraph
                // — assignment is intra-fn.
                let resolve = |lt: &crate::typeck::LifetimeRepr| -> Option<crate::borrowck::cfg::RegionId> {
                    lookup_or_static(&b.region_graph, lt)
                };
                let source = if is_return {
                    ConstraintSource::FnReturn
                } else {
                    ConstraintSource::Assign
                };
                emit_subtype_flow(
                    &src_ty,
                    &dst_ty,
                    Variance::Covariant,
                    &resolve,
                    &resolve,
                    b.ctx.structs,
                    b.ctx.enums,
                    &mut new_edges,
                    span,
                    source,
                );
                b.region_graph.outlives.extend(new_edges);
            }
            let _ = operand;
            let _ = OperandKind::ConstUnit;
        }
        Rvalue::Borrow { place: src, region: borrow_r, .. } => {
            // The borrow's region is `borrow_r`. If the source place is
            // itself a ref, the borrow is a reborrow: source's region
            // outlives the reborrow.
            if let Some(src_r) = place_outer_region(b, src) {
                b.region_graph.outlives.push(OutlivesConstraint {
                    sup: src_r,
                    sub: *borrow_r,
                    span: span.copy(),
                    source: ConstraintSource::Reborrow,
                });
            }
            // Borrow flows into LHS (or return).
            if is_return {
                if let Some(ret_r) = b.region_graph.fn_return_region {
                    b.region_graph.outlives.push(OutlivesConstraint {
                        sup: *borrow_r,
                        sub: ret_r,
                        span: span.copy(),
                        source: ConstraintSource::FnReturn,
                    });
                }
            } else if let Some(lhs_r) = lhs_region {
                b.region_graph.outlives.push(OutlivesConstraint {
                    sup: *borrow_r,
                    sub: lhs_r,
                    span: span.copy(),
                    source: ConstraintSource::Assign,
                });
            }
        }
        Rvalue::Call { callee, args, call_node_id } => {
            emit_call_constraints(b, callee, args, *call_node_id, lhs_place, lhs_region, is_return, span);
        }
        // Cast / StructLit / Tuple / Variant / Builtin / Discriminant
        // either don't carry region constraints (Cast erases) or aren't
        // covered by this MVP. Add as needs arise.
        _ => {}
    }
}

fn operand_region(b: &Builder, op: &crate::borrowck::cfg::Operand) -> Option<crate::borrowck::cfg::RegionId> {
    use crate::borrowck::cfg::OperandKind;
    match &op.kind {
        OperandKind::Move(p) | OperandKind::Copy(p) => place_outer_region(b, p),
        OperandKind::ConstStr(_) => Some(crate::borrowck::cfg::STATIC_REGION),
        OperandKind::ConstInt(_) | OperandKind::ConstBool(_) | OperandKind::ConstUnit => None,
    }
}

// Resolve an operand's full type (with projections applied). Used by
// `emit_subtype_flow` to walk the operand's type structure paired
// against the destination's type. Returns `None` for non-place
// operands (literals) — those carry no per-region constraints.
fn operand_type(b: &Builder, op: &crate::borrowck::cfg::Operand) -> Option<RType> {
    use crate::borrowck::cfg::OperandKind;
    match &op.kind {
        OperandKind::Move(p) | OperandKind::Copy(p) => place_type(b, p),
        // String literals: `&'static str` — represented inline so the
        // flow walker sees the right shape.
        OperandKind::ConstStr(_) => Some(RType::Ref {
            inner: Box::new(RType::Str),
            mutable: false,
            lifetime: crate::typeck::LifetimeRepr::Named("static".to_string()),
        }),
        OperandKind::ConstInt(_) | OperandKind::ConstBool(_) | OperandKind::ConstUnit => None,
    }
}

// Call-site region instantiation: for each callee free region (named
// lifetime + each `Inferred(N)` slot in param/return types), allocate
// a fresh RegionId in the caller. Substitute the callee's signature
// through this map. Emit:
//   * `arg_caller_region : arg_callee_region_inst` (CallArg) per arg
//     whose type carries an outermost region.
//   * `ret_callee_region_inst : ret_caller_region` (CallReturn) when
//     the call's return value has a region and is assigned to a place
//     with a known region (or returned directly).
//   * Each callee where-clause `'a : 'b` becomes
//     `'a_inst : 'b_inst` (WhereClause).
fn emit_call_constraints(
    b: &mut Builder,
    callee: &crate::borrowck::cfg::CallTarget,
    args: &Vec<crate::borrowck::cfg::Operand>,
    call_node_id: crate::ast::NodeId,
    lhs_place: &crate::borrowck::cfg::Place,
    lhs_region: Option<crate::borrowck::cfg::RegionId>,
    is_return: bool,
    span: &crate::span::Span,
) {
    use crate::borrowck::cfg::{CallTarget, ConstraintSource, OutlivesConstraint};
    // Resolve the callee's signature. For Path callees, the path is
    // direct. For MethodResolution callees, typeck recorded the
    // resolved callee path on `method_resolutions[node_id].callee_path`
    // (after dispatch + autoderef). Either way we end up with a
    // `Vec<String>` to look up in `funcs.entries` / `funcs.templates`.
    let path = match callee {
        CallTarget::Path(p) => p.clone(),
        CallTarget::MethodResolution(_) => {
            let mr = match b.ctx.method_resolutions.get(call_node_id as usize) {
                Some(Some(m)) => m,
                _ => return,
            };
            mr.callee_path.clone()
        }
    };
    let (param_types, return_type, lifetime_params, lifetime_predicates) =
        match crate::typeck::func_lookup(b.ctx.funcs, &path) {
            Some(e) => (
                e.param_types.clone(),
                e.return_type.clone(),
                e.lifetime_params.clone(),
                e.lifetime_predicates.clone(),
            ),
            None => match crate::typeck::template_lookup(b.ctx.funcs, &path) {
                Some((_, t)) => (
                    t.param_types.clone(),
                    t.return_type.clone(),
                    t.lifetime_params.clone(),
                    t.lifetime_predicates.clone(),
                ),
                None => return,
            },
        };
    // Build the substitution from callee region names → fresh caller
    // RegionIds. Named first; Inferred slots discovered while walking
    // param/return types (each unique `Inferred(N)` gets its own
    // fresh region, deduped by N).
    let mut named_subst: Vec<(String, crate::borrowck::cfg::RegionId)> = Vec::new();
    for n in &lifetime_params {
        let r = b.fresh_region();
        named_subst.push((n.clone(), r));
    }
    let mut inferred_subst: Vec<(u32, crate::borrowck::cfg::RegionId)> = Vec::new();
    for pt in &param_types {
        collect_callee_inferred(pt, &mut inferred_subst, &mut b.region_count);
    }
    if let Some(rt) = &return_type {
        collect_callee_inferred(rt, &mut inferred_subst, &mut b.region_count);
    }
    // Per-arg: `arg_region : callee_param_region_inst`.
    let mut i = 0;
    while i < args.len() && i < param_types.len() {
        let arg_r = operand_region(b, &args[i]);
        let pt_r = type_outer_region_subst(&param_types[i], &named_subst, &inferred_subst);
        if let (Some(ar), Some(pr)) = (arg_r, pt_r) {
            b.region_graph.outlives.push(OutlivesConstraint {
                sup: ar,
                sub: pr,
                span: span.copy(),
                source: ConstraintSource::CallArg,
            });
        }
        i += 1;
    }
    // Where-clauses: each `'a : 'b` becomes `'a_inst : 'b_inst`.
    for pred in &lifetime_predicates {
        let sup = subst_lookup_named(&pred.lhs, &named_subst);
        if sup.is_none() {
            continue;
        }
        for sub_name in &pred.bounds {
            if let Some(sub) = subst_lookup_named(sub_name, &named_subst) {
                b.region_graph.outlives.push(OutlivesConstraint {
                    sup: sup.unwrap(),
                    sub,
                    span: span.copy(),
                    source: ConstraintSource::WhereClause,
                });
            }
        }
    }
    // Return value's region flows into LHS (or fn return).
    if let Some(ret_ty) = &return_type {
        let ret_r = type_outer_region_subst(ret_ty, &named_subst, &inferred_subst);
        if let Some(rr) = ret_r {
            if is_return {
                if let Some(fr) = b.region_graph.fn_return_region {
                    b.region_graph.outlives.push(OutlivesConstraint {
                        sup: rr,
                        sub: fr,
                        span: span.copy(),
                        source: ConstraintSource::CallReturn,
                    });
                }
            } else if let Some(lr) = lhs_region {
                b.region_graph.outlives.push(OutlivesConstraint {
                    sup: rr,
                    sub: lr,
                    span: span.copy(),
                    source: ConstraintSource::CallReturn,
                });
            }
        }
    }
    let _ = lhs_place;
}

fn collect_callee_inferred(
    rt: &RType,
    out: &mut Vec<(u32, crate::borrowck::cfg::RegionId)>,
    counter: &mut u32,
) {
    use crate::typeck::LifetimeRepr;
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            if let LifetimeRepr::Inferred(id) = lifetime {
                if *id != 0 && !out.iter().any(|(n, _)| *n == *id) {
                    let r = *counter;
                    *counter += 1;
                    out.push((*id, r));
                }
            }
            collect_callee_inferred(inner, out, counter);
        }
        RType::RawPtr { inner, .. } => collect_callee_inferred(inner, out, counter),
        RType::Struct { type_args, lifetime_args, .. }
        | RType::Enum { type_args, lifetime_args, .. } => {
            for la in lifetime_args {
                if let LifetimeRepr::Inferred(id) = la {
                    if *id != 0 && !out.iter().any(|(n, _)| *n == *id) {
                        let r = *counter;
                        *counter += 1;
                        out.push((*id, r));
                    }
                }
            }
            for ta in type_args {
                collect_callee_inferred(ta, out, counter);
            }
        }
        RType::Tuple(elems) => {
            for e in elems {
                collect_callee_inferred(e, out, counter);
            }
        }
        RType::Slice(inner) => collect_callee_inferred(inner, out, counter),
        RType::AssocProj { base, .. } => collect_callee_inferred(base, out, counter),
        RType::Bool
        | RType::Int(_)
        | RType::Char
        | RType::Str
        | RType::Never
        | RType::Param(_)
        | RType::Opaque { .. } => {}
        RType::FnPtr { params, ret } => {
            for p in params {
                collect_callee_inferred(p, out, counter);
            }
            collect_callee_inferred(ret, out, counter);
        }
        RType::Dyn { lifetime, .. } => {
            if let LifetimeRepr::Inferred(id) = lifetime {
                if *id != 0 && !out.iter().any(|(n, _)| *n == *id) {
                    let r = *counter;
                    *counter += 1;
                    out.push((*id, r));
                }
            }
        }
    }
}

fn subst_lookup_named(
    name: &str,
    named_subst: &Vec<(String, crate::borrowck::cfg::RegionId)>,
) -> Option<crate::borrowck::cfg::RegionId> {
    if name == "static" {
        return Some(crate::borrowck::cfg::STATIC_REGION);
    }
    for (n, r) in named_subst {
        if n == name {
            return Some(*r);
        }
    }
    None
}

fn type_outer_region_subst(
    rt: &RType,
    named_subst: &Vec<(String, crate::borrowck::cfg::RegionId)>,
    inferred_subst: &Vec<(u32, crate::borrowck::cfg::RegionId)>,
) -> Option<crate::borrowck::cfg::RegionId> {
    use crate::typeck::LifetimeRepr;
    match rt {
        RType::Ref { lifetime, .. } => match lifetime {
            LifetimeRepr::Named(name) => subst_lookup_named(name, named_subst),
            LifetimeRepr::Inferred(id) => {
                for (n, r) in inferred_subst {
                    if *n == *id {
                        return Some(*r);
                    }
                }
                None
            }
        },
        _ => None,
    }
}

impl<'a> Builder<'a> {
    fn new_block(&mut self) -> BlockId {
        let id = self.blocks.len() as BlockId;
        self.blocks.push(BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    fn push_stmt(&mut self, kind: CfgStmtKind, span: Span) {
        self.blocks[self.current_block as usize]
            .stmts
            .push(CfgStmt { kind, span });
    }

    fn set_terminator(&mut self, t: Terminator) {
        self.blocks[self.current_block as usize].terminator = t;
    }

    fn alloc_local(
        &mut self,
        name: Option<String>,
        ty: RType,
        span: Span,
        mutable: bool,
        is_temp: bool,
    ) -> LocalId {
        let id = self.locals.len() as LocalId;
        self.locals.push(LocalDecl {
            name,
            ty,
            span,
            mutable,
            is_temp,
        });
        id
    }

    fn alloc_temp(&mut self, ty: RType, span: Span) -> LocalId {
        self.alloc_local(None, ty, span, true, true)
    }

    fn fresh_region(&mut self) -> RegionId {
        let r = self.region_count;
        self.region_count += 1;
        r
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope { names: Vec::new() });
    }

    fn pop_scope(&mut self) {
        if let Some(s) = self.scopes.pop() {
            // Emit StorageDead in declaration-reverse order at scope exit.
            let mut i = s.names.len();
            while i > 0 {
                i -= 1;
                let (_name, local) = s.names[i].clone();
                self.push_stmt(
                    CfgStmtKind::StorageDead(local),
                    self.locals[local as usize].span.copy(),
                );
            }
        }
    }

    fn bind_name(&mut self, name: &str, local: LocalId) {
        let s = self.scopes.last_mut().expect("scope must be active");
        s.names.push((name.to_string(), local));
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        let mut i = self.scopes.len();
        while i > 0 {
            i -= 1;
            let s = &self.scopes[i];
            let mut j = s.names.len();
            while j > 0 {
                j -= 1;
                if s.names[j].0 == name {
                    return Some(s.names[j].1);
                }
            }
        }
        None
    }

    // Lower a block. If `target` is Some, the block's tail expression
    // is assigned to it; otherwise the tail (if any) is evaluated and
    // discarded.
    fn lower_block(&mut self, block: &Block, target: Option<Place>) {
        self.push_scope();
        let mut i = 0;
        while i < block.stmts.len() {
            self.lower_stmt(&block.stmts[i]);
            i += 1;
        }
        if let Some(tail) = &block.tail {
            if let Some(t) = target.clone() {
                self.lower_expr_into(tail, t);
            } else {
                let _ = self.lower_expr_operand(tail);
            }
        }
        self.pop_scope();
    }

    fn lower_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(let_stmt) => self.lower_let(let_stmt),
            Stmt::Assign(assign) => self.lower_assign(assign),
            Stmt::Expr(expr) => {
                let _ = self.lower_expr_operand(expr);
            }
            Stmt::Use(_) => {}
        }
    }

    fn lower_let(&mut self, ls: &LetStmt) {
        // `let x: T;` (uninit): typeck has already validated the
        // single-Binding pattern + present annotation. The binding's
        // type is recorded by typeck at `pattern.id`. Emit
        // StorageLive + Uninit so the move dataflow marks the
        // place as Moved at the let-stmt; reads before the first
        // assignment surface as "use of uninitialized" diagnostics.
        if ls.value.is_none() {
            let (name, mutable, name_span) = crate::ast::let_simple_binding(ls)
                .expect("typeck enforces single Binding for uninit let");
            let ty = self.expr_type(ls.pattern.id);
            let id = self.alloc_local(
                Some(name.to_string()),
                ty,
                name_span.copy(),
                mutable,
                false,
            );
            self.push_stmt(CfgStmtKind::StorageLive(id), name_span.copy());
            self.push_stmt(CfgStmtKind::Uninit(id), name_span.copy());
            self.bind_name(name, id);
            return;
        }
        let value_expr = ls.value.as_ref().expect("just checked is_some");
        let ty = self.expr_type(value_expr.id);
        // Simple-binding fast path: `let x = e;` / `let mut x = e;`.
        // Tuple/struct destructure and let-else go through the
        // pattern-binding path below.
        if let Some((name, mutable, name_span)) = crate::ast::let_simple_binding(ls) {
            let id = self.alloc_local(
                Some(name.to_string()),
                ty.clone(),
                name_span.copy(),
                mutable,
                false,
            );
            self.push_stmt(CfgStmtKind::StorageLive(id), name_span.copy());
            let place = local_place(id);
            self.lower_expr_into(value_expr, place);
            self.bind_name(name, id);
            return;
        }
        // General pattern: lower the value into a temp, then walk
        // the pattern to bind sub-places. (let-else, tuple destructure,
        // wildcard `let _ = e;`, etc.) Note: we rely on typeck having
        // rejected refutable patterns without a let-else.
        let scrut = self.alloc_temp(ty.clone(), value_expr.span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(scrut), value_expr.span.copy());
        self.lower_expr_into(value_expr, local_place(scrut));
        let scrut_place = local_place(scrut);
        // For let-else: emit a pattern test; on no-match, lower the
        // else-block (which must diverge per typeck) so we never fall
        // through.
        if let Some(else_block) = &ls.else_block {
            let then_block = self.new_block();
            let else_target = self.new_block();
            self.lower_pattern_test(
                &ls.pattern,
                &scrut_place,
                &ty,
                then_block,
                else_target,
            );
            self.current_block = else_target;
            self.lower_block(else_block.as_ref(), None);
            // Else block diverges (typeck-enforced) — but emit a
            // safety terminator in case lowering didn't already.
            self.set_terminator(super::cfg::Terminator::Unreachable);
            self.current_block = then_block;
        }
        let bindings = self.collect_pattern_bindings(&ls.pattern, &scrut_place, &ty);
        self.materialize_bindings(&bindings);
    }

    fn lower_assign(&mut self, a: &AssignStmt) {
        let place = self.lower_expr_place(&a.lhs);
        self.lower_expr_into(&a.rhs, place);
    }

    // Lower `expr` and store its value into `dest`.
    fn lower_expr_into(&mut self, expr: &Expr, dest: Place) {
        match &expr.kind {
            ExprKind::If(if_expr) => {
                self.lower_if_into(if_expr, expr.span.copy(), Some(dest));
            }
            ExprKind::Match(m) => {
                self.lower_match_into(m, Some(dest));
            }
            ExprKind::IfLet(il) => {
                self.lower_if_let_into(il, Some(dest));
            }
            ExprKind::While(w) => {
                self.lower_while(w, expr.span.copy());
                // The while expression evaluates to (); init dest with
                // unit (no-op for unit dest, but explicit for sanity).
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue: Rvalue::Use(Operand {
                            kind: OperandKind::ConstUnit,
                            span: expr.span.copy(),
                            node_id: Some(expr.id),
                        }),
                    },
                    expr.span.copy(),
                );
            }
            ExprKind::For(f) => {
                self.lower_for(f, expr.span.copy());
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue: Rvalue::Use(Operand {
                            kind: OperandKind::ConstUnit,
                            span: expr.span.copy(),
                            node_id: Some(expr.id),
                        }),
                    },
                    expr.span.copy(),
                );
            }
            ExprKind::Break { label, label_span: _ } => {
                self.lower_break(label, expr.span.copy());
            }
            ExprKind::Continue { label, label_span: _ } => {
                self.lower_continue(label, expr.span.copy());
            }
            ExprKind::Return { value } => {
                self.lower_return(value.as_deref(), expr.span.copy());
            }
            ExprKind::Try { inner, .. } => {
                // The `?` operator's "happy path" extracts the Ok payload
                // into `dest`; the error path returns the function early.
                // For borrowck purposes we model it as: read inner once
                // (non-trivially — the Result is consumed), then assign
                // the Ok payload to `dest`. The Err-return diverges; we
                // don't need to thread it through borrowck since the
                // function ends at that point.
                let _ = self.lower_expr_operand(inner);
                // Synthesize an Operand-of-Ok-payload by reading the
                // inner's place — since we already lowered the inner
                // for its side effects, re-treat its place as the
                // source. Conservative.
                let rvalue = self.lower_expr_rvalue(inner);
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue,
                    },
                    expr.span.copy(),
                );
            }
            ExprKind::Block(b) => {
                self.lower_block(b.as_ref(), Some(dest));
            }
            ExprKind::Unsafe(b) => {
                self.lower_block(b.as_ref(), Some(dest));
            }
            // For all other expressions, lower to an rvalue and assign.
            _ => {
                let rvalue = self.lower_expr_rvalue(expr);
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue,
                    },
                    expr.span.copy(),
                );
            }
        }
    }

    // Lower an expression to an Operand (move/copy of a place, or a
    // constant). Compound exprs go through a temporary.
    fn lower_expr_operand(&mut self, expr: &Expr) -> Operand {
        let span = expr.span.copy();
        let nid = Some(expr.id);
        match &expr.kind {
            ExprKind::IntLit(n) => Operand { kind: OperandKind::ConstInt(*n), span, node_id: nid },
            // CFG-level borrowck doesn't do arithmetic; the sign of
            // a literal carries no move/borrow semantics, so we treat
            // `NegIntLit(n)` as just another constant operand.
            ExprKind::NegIntLit(n) => Operand { kind: OperandKind::ConstInt(*n), span, node_id: nid },
            ExprKind::StrLit(s) => Operand { kind: OperandKind::ConstStr(s.clone()), span, node_id: nid },
            ExprKind::BoolLit(b) => Operand { kind: OperandKind::ConstBool(*b), span, node_id: nid },
            // Char literals reduce to a 4-byte i32 codepoint — for
            // CFG-level borrowck purposes the same as an integer
            // constant.
            ExprKind::CharLit(c) => Operand { kind: OperandKind::ConstInt(*c as u64), span, node_id: nid },
            ExprKind::Tuple(elems) if elems.is_empty() => {
                Operand { kind: OperandKind::ConstUnit, span, node_id: nid }
            }
            ExprKind::Var(name) => {
                // Const-reference fallback: typeck records const uses
                // on `const_uses[id]`; those Vars have no local.
                // Lower them to a constant operand carrying the
                // value's payload so move/borrow analyses see no
                // place to track. Locals checked first — typeck
                // ensures the two are mutually exclusive.
                if let Some(opt) = self.ctx.const_uses.get(expr.id as usize) {
                    if let Some(value) = opt {
                        use crate::typeck::ConstValue;
                        let kind = match value {
                            ConstValue::Int { magnitude, negated } => {
                                let signed = if *negated {
                                    (*magnitude as i64).wrapping_neg() as u64
                                } else {
                                    *magnitude
                                };
                                OperandKind::ConstInt(signed)
                            }
                            ConstValue::Bool(b) => OperandKind::ConstBool(*b),
                            ConstValue::Char(c) => OperandKind::ConstInt(*c as u64),
                            ConstValue::Str(s) => OperandKind::ConstStr(s.clone()),
                        };
                        return Operand { kind, span, node_id: nid };
                    }
                }
                // Fn-item-address fallback: typeck records a callee_idx
                // when a bare-name Var coerces into an FnPtr slot. The
                // value is a Copy i32 (funcref-table slot resolved at
                // codegen); borrowck has no place to track, so emit a
                // ConstInt(0) placeholder. The actual slot value is
                // computed in codegen via `intern_table_slot`.
                if let Some(opt) = self.ctx.fn_item_addrs.get(expr.id as usize) {
                    if opt.is_some() {
                        return Operand { kind: OperandKind::ConstInt(0), span, node_id: nid };
                    }
                }
                let local = self.lookup(name).expect("typeck verified");
                let place = local_place(local);
                let ty = self.locals[local as usize].ty.clone();
                self.materialize_if_move(&place, ty, span, nid)
            }
            ExprKind::FieldAccess(_)
            | ExprKind::TupleIndex { .. }
            | ExprKind::Deref(_) => {
                let place = self.lower_expr_place(expr);
                let ty = self.expr_type(expr.id);
                self.materialize_if_move(&place, ty, span, nid)
            }
            _ => {
                // Other expressions need a temporary.
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(
                    CfgStmtKind::StorageLive(temp),
                    expr.span.copy(),
                );
                self.lower_expr_into(expr, local_place(temp));
                // Reading from a temp isn't a source-level binding
                // move, so don't pin a node_id (drop-flag synthesis
                // would never look at temps anyway).
                self.move_or_copy(&local_place(temp), ty, span, None)
            }
        }
    }

    fn move_or_copy(
        &self,
        place: &Place,
        ty: RType,
        span: Span,
        node_id: Option<ast::NodeId>,
    ) -> Operand {
        let copy = is_copy_with_bounds(
            &ty,
            self.ctx.traits,
            self.ctx.type_params,
            self.ctx.type_param_bounds,
        );
        let kind = if copy {
            OperandKind::Copy(place.clone())
        } else {
            OperandKind::Move(place.clone())
        };
        Operand { kind, span, node_id }
    }

    // For a place-shaped operand: if the operand would be a Move
    // (non-Copy), emit `temp = move place` here so the move-effect lands
    // in the CFG at source-evaluation order — not deferred to the outer
    // statement that consumes the operand. Without this, e.g.
    // `f(o.x, g(&o))` would record both args' effects in the outer
    // Call's stmt, with `&o` (lowered through nested temps) processed
    // first and `move o.x` processed last; subsequent borrows that
    // should fail because of the move would spuriously succeed.
    //
    // Copy reads need no materialization (they have no move-effect, and
    // reading a Copy place again later is fine).
    fn materialize_if_move(
        &mut self,
        place: &Place,
        ty: RType,
        span: Span,
        node_id: Option<ast::NodeId>,
    ) -> Operand {
        let copy = is_copy_with_bounds(
            &ty,
            self.ctx.traits,
            self.ctx.type_params,
            self.ctx.type_param_bounds,
        );
        // Implicit function/builtin-arg reborrow for `&mut T`: same
        // justification as method-receiver reborrow (`lower_recv_reborrow`).
        // The call scope-bounds the borrow; after the call, the original
        // `&mut T` binding is usable again. Without this, calling two
        // builtins/funcs with the same `&mut self` arg in a method body
        // (e.g. `IndexMut for str`'s body using `¤str_len(self)` then
        // `¤str_as_mut_bytes(self)`) errors with "self already moved".
        // Sound because pocket-rust treats the reborrow as exclusive
        // for the duration of the call, just like Rust does.
        let mut_ref_reborrow = matches!(&ty, RType::Ref { mutable: true, .. });
        if copy || mut_ref_reborrow {
            return Operand {
                kind: OperandKind::Copy(place.clone()),
                span,
                node_id,
            };
        }
        let temp = self.alloc_temp(ty.clone(), span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(temp), span.copy());
        let move_op = Operand {
            kind: OperandKind::Move(place.clone()),
            span: span.copy(),
            node_id,
        };
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(temp),
                rvalue: Rvalue::Use(move_op),
            },
            span.copy(),
        );
        Operand {
            kind: OperandKind::Move(local_place(temp)),
            span,
            node_id: None,
        }
    }

    // Lower an expression to a Place (used as borrow target or
    // assignment LHS). Only place-shaped expressions are valid here:
    // Var, FieldAccess, TupleIndex, Deref.
    fn lower_expr_place(&mut self, expr: &Expr) -> Place {
        match &expr.kind {
            ExprKind::Var(name) => {
                // Const place fallback: typeck records const-receiver
                // uses (e.g. `CONST.method()` or `CONST + n` whose
                // desugar puts CONST in receiver-place position) on
                // `const_uses[id]`. Materialize the value into a
                // synthetic temp local + initial assignment so
                // downstream borrowck has a real Place to track. The
                // temp's type is the const's, derived from
                // `expr_types`.
                if let Some(opt) = self.ctx.const_uses.get(expr.id as usize) {
                    if let Some(value) = opt {
                        return self.materialize_const_place(value, expr);
                    }
                }
                let local = self.lookup(name).expect("typeck verified");
                local_place(local)
            }
            ExprKind::FieldAccess(fa) => {
                let mut p = self.lower_expr_place(&fa.base);
                p.projections.push(Projection::Field(fa.field.clone()));
                p
            }
            ExprKind::TupleIndex { base, index, .. } => {
                let mut p = self.lower_expr_place(base);
                p.projections.push(Projection::TupleIndex(*index));
                p
            }
            ExprKind::Deref(inner) => {
                // For `*expr`, we need `expr` to become an addressable
                // place but we shouldn't move it — the ref is just
                // being used to compute the deref address. Treat as
                // an implicit reborrow: when `inner` is itself a place
                // expression (Var/FieldAccess/Deref/etc.) we read its
                // place directly and append `Projection::Deref`,
                // without recording any move on the ref. This is what
                // lets `*self = ¤u8_add(*self, other);` typecheck —
                // both `*self` reads use `self` without consuming it.
                // Non-place inners (e.g. `*foo()`) still go through
                // operand materialization.
                if matches!(
                    inner.kind,
                    ExprKind::Var(_)
                        | ExprKind::FieldAccess(_)
                        | ExprKind::TupleIndex { .. }
                        | ExprKind::Deref(_)
                ) {
                    let mut p = self.lower_expr_place(inner);
                    p.projections.push(Projection::Deref);
                    p
                } else {
                    let op = self.lower_expr_operand(inner);
                    match op.kind {
                        OperandKind::Move(p) | OperandKind::Copy(p) => {
                            let mut p = p;
                            p.projections.push(Projection::Deref);
                            p
                        }
                        _ => unreachable!("typeck rejects deref of constant"),
                    }
                }
            }
            // Non-place expression appearing where a place is needed
            // (e.g., `f().field`): materialize into a fresh temp and
            // return the temp's place.
            _ => {
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_expr_into(expr, local_place(temp));
                local_place(temp)
            }
        }
    }

    // Lower an expression to an Rvalue (suitable for the RHS of
    // Assign). Place-only expressions return Use(Operand).
    fn lower_expr_rvalue(&mut self, expr: &Expr) -> Rvalue {
        let span = expr.span.copy();
        let nid = Some(expr.id);
        match &expr.kind {
            ExprKind::IntLit(n) => Rvalue::Use(Operand {
                kind: OperandKind::ConstInt(*n),
                span,
                node_id: nid,
            }),
            ExprKind::NegIntLit(n) => Rvalue::Use(Operand {
                kind: OperandKind::ConstInt(*n),
                span,
                node_id: nid,
            }),
            ExprKind::StrLit(s) => Rvalue::Use(Operand {
                kind: OperandKind::ConstStr(s.clone()),
                span,
                node_id: nid,
            }),
            ExprKind::BoolLit(b) => Rvalue::Use(Operand {
                kind: OperandKind::ConstBool(*b),
                span,
                node_id: nid,
            }),
            ExprKind::CharLit(c) => Rvalue::Use(Operand {
                kind: OperandKind::ConstInt(*c as u64),
                span,
                node_id: nid,
            }),
            ExprKind::Tuple(elems) if elems.is_empty() => Rvalue::Use(Operand {
                kind: OperandKind::ConstUnit,
                span,
                node_id: nid,
            }),
            ExprKind::Var(_)
            | ExprKind::FieldAccess(_)
            | ExprKind::TupleIndex { .. }
            | ExprKind::Deref(_) => {
                let op = self.lower_expr_operand(expr);
                Rvalue::Use(op)
            }
            ExprKind::Borrow { inner, mutable } => {
                let place = self.lower_expr_place(inner);
                let region = self.fresh_region();
                Rvalue::Borrow {
                    mutable: *mutable,
                    place,
                    region,
                }
            }
            ExprKind::Cast { inner, .. } => {
                let source = self.lower_expr_operand(inner);
                let target_ty = self.expr_type(expr.id);
                Rvalue::Cast { source, target_ty }
            }
            ExprKind::Tuple(elems) => {
                let mut ops: Vec<Operand> = Vec::new();
                let mut i = 0;
                while i < elems.len() {
                    ops.push(self.lower_expr_operand(&elems[i]));
                    i += 1;
                }
                Rvalue::Tuple(ops)
            }
            ExprKind::Builtin { name, args, .. } => {
                let mut ops: Vec<Operand> = Vec::new();
                let mut i = 0;
                while i < args.len() {
                    ops.push(self.lower_expr_operand(&args[i]));
                    i += 1;
                }
                Rvalue::Builtin {
                    name: name.clone(),
                    args: ops,
                }
            }
            ExprKind::Call(c) => self.lower_call(c, expr.id),
            ExprKind::MethodCall(mc) => self.lower_method_call(mc, expr.id),
            ExprKind::StructLit(lit) => self.lower_struct_lit(lit, expr.id),
            ExprKind::If(_) | ExprKind::Block(_) | ExprKind::Unsafe(_) => {
                // Should be handled by lower_expr_into; this path is for
                // places where rvalue is required (e.g., as a sub-rvalue
                // of an assign). For now, materialize through a temp.
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_expr_into(expr, local_place(temp));
                Rvalue::Use(self.move_or_copy(&local_place(temp), ty, span, None))
            }
            ExprKind::Match(m) => {
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_match_into(m, Some(local_place(temp)));
                Rvalue::Use(self.move_or_copy(&local_place(temp), ty, span, None))
            }
            ExprKind::IfLet(il) => {
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_if_let_into(il, Some(local_place(temp)));
                Rvalue::Use(self.move_or_copy(&local_place(temp), ty, span, None))
            }
            ExprKind::While(w) => {
                self.lower_while(w, expr.span.copy());
                // While expressions evaluate to (). Return a unit
                // operand wrapped as Use.
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::For(f) => {
                self.lower_for(f, expr.span.copy());
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Break { label, label_span: _ } => {
                self.lower_break(label, expr.span.copy());
                // Return value won't be reached at runtime; produce a
                // unit so the rvalue typing flows.
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Continue { label, label_span: _ } => {
                self.lower_continue(label, expr.span.copy());
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Return { value } => {
                self.lower_return(value.as_deref(), expr.span.copy());
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Try { inner, .. } => {
                // Same modeling as the assign-position case: read the
                // inner once. Borrowck doesn't need to differentiate
                // the Ok/Err split — typeck has verified the shapes.
                self.lower_expr_rvalue(inner)
            }
            ExprKind::Index { base, index, .. } => {
                // Model `arr[idx]` as `Index::index(&arr, idx)` for
                // borrowck purposes — base is *borrowed*, not moved,
                // so subsequent uses of base remain valid.
                let _recv = self.synth_borrow(base, false);
                let _ = self.lower_expr_operand(index);
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::MacroCall { args, .. } => {
                // panic!(msg) — read the message arg; the call
                // diverges, so subsequent code in the same block is
                // unreachable for borrowck.
                let mut i = 0;
                while i < args.len() {
                    let _ = self.lower_expr_operand(&args[i]);
                    i += 1;
                }
                self.set_terminator(Terminator::Unreachable);
                let unreachable = self.new_block();
                self.current_block = unreachable;
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Closure(_) => {
                unreachable!("closure expressions rejected at typeck before borrowck")
            }
        }
    }

    fn lower_call(&mut self, c: &Call, node_id: ast::NodeId) -> Rvalue {
        // Bare-closure / bare-typeparam call (`f(args)` where `f` is a
        // local with synthesized closure type or a `F: Fn*`-bounded
        // type-param). Typeck records the binding name on
        // `bare_closure_calls[id]` and the dispatch's `recv_adjust` on
        // `method_resolutions[id]`. Borrowck synthesizes the receiver
        // effect HERE — without this, the binding's move-out from a
        // FnOnce dispatch never lands in the CFG, and `f(); f();`
        // silently slips through as use-after-move. Generic over the
        // dispatched trait: any future `recv_adjust = Move` on a Param
        // -typed receiver gets correct treatment automatically.
        let bare_recv = self
            .ctx
            .bare_closure_calls
            .get(node_id as usize)
            .and_then(|o| o.as_ref());
        let bare_recv_op: Option<Operand> = if let Some(name) = bare_recv {
            let local = self.lookup(name).expect("typeck verified binding in scope");
            let ty = self.ctx.method_resolutions[node_id as usize]
                .as_ref()
                .map(|r| r.recv_adjust)
                .unwrap_or(ReceiverAdjust::Move);
            let span = c.callee.span.copy();
            let op = match ty {
                ReceiverAdjust::Move => Operand {
                    kind: OperandKind::Move(local_place(local)),
                    span,
                    node_id: Some(node_id),
                },
                ReceiverAdjust::BorrowImm | ReceiverAdjust::BorrowMut | ReceiverAdjust::ByRef => {
                    Operand {
                        kind: OperandKind::Copy(local_place(local)),
                        span,
                        node_id: Some(node_id),
                    }
                }
            };
            Some(op)
        } else {
            None
        };
        let mut args: Vec<Operand> = Vec::new();
        if let Some(op) = bare_recv_op {
            args.push(op);
        }
        let mut i = 0;
        while i < c.args.len() {
            args.push(self.lower_expr_operand(&c.args[i]));
            i += 1;
        }
        let resolution = self.ctx.call_resolutions[node_id as usize].as_ref();
        let callee = match resolution {
            Some(CallResolution::Direct(idx)) => {
                CallTarget::Path(self.ctx.funcs.entries[*idx].path.clone())
            }
            Some(CallResolution::Generic { template_idx, .. }) => {
                CallTarget::Path(self.ctx.funcs.templates[*template_idx].path.clone())
            }
            Some(CallResolution::Variant {
                enum_path,
                disc,
                type_args,
            }) => {
                return Rvalue::Variant {
                    enum_path: enum_path.clone(),
                    type_args: type_args.clone(),
                    disc: *disc,
                    fields: VariantFields::Tuple(args),
                };
            }
            Some(CallResolution::Indirect { callee_local_name, .. }) => {
                // Indirect call through an FnPtr local — borrowck has
                // no signature-level lifetime relationships to trace
                // (the FnPtr value itself is Copy + carries no borrow
                // edges), so synthesize a single-segment placeholder
                // path with the local name. Borrow propagation through
                // args still happens via `args` above.
                CallTarget::Path(vec![callee_local_name.clone()])
            }
            None => {
                // Unresolved call — typeck would have errored. Use a
                // placeholder path.
                CallTarget::Path(c.callee.segments.iter().map(|s| s.name.clone()).collect())
            }
        };
        Rvalue::Call {
            callee,
            args,
            call_node_id: node_id,
        }
    }

    fn lower_method_call(&mut self, mc: &MethodCall, node_id: ast::NodeId) -> Rvalue {
        // Receiver lowering depends on the method's receiver-adjust:
        // BorrowImm/BorrowMut take an implicit borrow (== `(&recv)` /
        // `(&mut recv)`); Move consumes the owned value; ByRef passes
        // an existing reference through.
        let recv_adjust = self.ctx.method_resolutions[node_id as usize]
            .as_ref()
            .map(|r| r.recv_adjust)
            .unwrap_or(ReceiverAdjust::Move);
        let recv = match recv_adjust {
            ReceiverAdjust::BorrowImm => self.synth_borrow(&mc.receiver, false),
            ReceiverAdjust::BorrowMut => self.synth_borrow(&mc.receiver, true),
            ReceiverAdjust::Move => self.lower_expr_operand(&mc.receiver),
            // ByRef: the receiver is already a ref (`&T` or `&mut T`)
            // and the method takes a ref-typed self. Lower as a
            // **reborrow**: we copy the receiver's i32 ref value into
            // the call instead of consuming the binding. Semantically
            // valid because the callee only borrows for the call's
            // duration; after the call the source binding resumes.
            // Without this, `&mut self` methods couldn't transitively
            // call other `&mut self` methods on the same binding.
            ReceiverAdjust::ByRef => self.lower_recv_reborrow(&mc.receiver),
        };
        let mut args: Vec<Operand> = Vec::new();
        args.push(recv);
        let mut i = 0;
        while i < mc.args.len() {
            args.push(self.lower_expr_operand(&mc.args[i]));
            i += 1;
        }
        Rvalue::Call {
            callee: CallTarget::MethodResolution(node_id),
            args,
            call_node_id: node_id,
        }
    }

    // Reborrow lowering for a method-call receiver that is already a
    // ref (`&T` or `&mut T`). Always emits `Operand::Copy(place)` —
    // even when the place's resolved type is `&mut T` (which is not
    // Copy), copying its i32 representation is sound because the
    // callee scope-bounds the borrow. The original ref-typed binding
    // remains live for use after the call.
    fn lower_recv_reborrow(&mut self, expr: &Expr) -> Operand {
        let span = expr.span.copy();
        let nid = Some(expr.id);
        let place = self.lower_expr_place(expr);
        Operand {
            kind: OperandKind::Copy(place),
            span,
            node_id: nid,
        }
    }

    // Take an implicit borrow of `expr` (used by autoref method
    // dispatch). Materializes through a temp so the call's argument
    // list is uniform Operands.
    fn synth_borrow(&mut self, expr: &Expr, mutable: bool) -> Operand {
        let span = expr.span.copy();
        let place = self.lower_expr_place(expr);
        let region = self.fresh_region();
        let inner_ty = self.expr_type(expr.id);
        let ref_ty = RType::Ref {
            inner: Box::new(inner_ty),
            mutable,
            lifetime: LifetimeRepr::Inferred(0),
        };
        let temp = self.alloc_temp(ref_ty, span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(temp), span.copy());
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(temp),
                rvalue: Rvalue::Borrow {
                    mutable,
                    place,
                    region,
                },
            },
            span.copy(),
        );
        Operand {
            kind: OperandKind::Move(local_place(temp)),
            span,
            node_id: None,
        }
    }

    // Materialize a const value into a synthetic temp local so
    // place-position uses (binop receivers, method receivers,
    // `&CONST` borrows) have a real Place to track. The temp's type
    // is read from `expr_types` (recorded by typeck at the Var's
    // NodeId). The initial assignment carries the const's payload as
    // an `Operand::Const*`.
    fn materialize_const_place(
        &mut self,
        value: &crate::typeck::ConstValue,
        expr: &Expr,
    ) -> Place {
        use crate::typeck::ConstValue;
        let span = expr.span.copy();
        let ty = self.expr_type(expr.id);
        let temp = self.alloc_temp(ty, span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(temp), span.copy());
        let kind = match value {
            ConstValue::Int { magnitude, negated } => {
                let signed = if *negated {
                    (*magnitude as i64).wrapping_neg() as u64
                } else {
                    *magnitude
                };
                OperandKind::ConstInt(signed)
            }
            ConstValue::Bool(b) => OperandKind::ConstBool(*b),
            ConstValue::Char(c) => OperandKind::ConstInt(*c as u64),
            ConstValue::Str(s) => OperandKind::ConstStr(s.clone()),
        };
        let init_op = Operand { kind, span: span.copy(), node_id: Some(expr.id) };
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(temp),
                rvalue: Rvalue::Use(init_op),
            },
            span.copy(),
        );
        local_place(temp)
    }

    fn lower_struct_lit(&mut self, lit: &StructLit, node_id: ast::NodeId) -> Rvalue {
        let mut fields: Vec<(String, Operand)> = Vec::new();
        let mut i = 0;
        while i < lit.fields.len() {
            let op = self.lower_expr_operand(&lit.fields[i].value);
            fields.push((lit.fields[i].name.clone(), op));
            i += 1;
        }
        // Struct literal might also be an enum variant struct-form.
        let resolution = self.ctx.call_resolutions[node_id as usize].as_ref();
        if let Some(CallResolution::Variant {
            enum_path,
            disc,
            type_args,
        }) = resolution
        {
            return Rvalue::Variant {
                enum_path: enum_path.clone(),
                type_args: type_args.clone(),
                disc: *disc,
                fields: VariantFields::Struct(fields),
            };
        }
        // Pull out type info from expr_types for type_args.
        let ty = self.expr_type(node_id);
        let (type_path, type_args) = match &ty {
            RType::Struct {
                path, type_args, ..
            } => (path.clone(), type_args.clone()),
            _ => unreachable!("struct lit expr type must be a Struct"),
        };
        Rvalue::StructLit {
            type_path,
            type_args,
            fields,
        }
    }

    // Lower an `if` expression. `target` is Some when the if is on the
    // RHS of an assign / let / has a containing destination.
    fn lower_if_into(
        &mut self,
        if_expr: &ast::IfExpr,
        span: Span,
        target: Option<Place>,
    ) {
        let cond = self.lower_expr_operand(&if_expr.cond);
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge_block = self.new_block();
        self.set_terminator(Terminator::If {
            cond,
            then_block,
            else_block,
        });

        self.current_block = then_block;
        self.lower_block(if_expr.then_block.as_ref(), target.clone());
        self.set_terminator(Terminator::Goto(merge_block));

        self.current_block = else_block;
        self.lower_block(if_expr.else_block.as_ref(), target);
        self.set_terminator(Terminator::Goto(merge_block));

        self.current_block = merge_block;
        let _ = span;
    }

    fn expr_type(&self, id: ast::NodeId) -> RType {
        self.ctx.expr_types[id as usize]
            .as_ref()
            .expect("typeck recorded this expr's type")
            .clone()
    }

    // ---------- Match / if-let lowering ----------

    // Lower a match expression. Each arm: pattern test → arm body block
    // → goto merge. Bindings inside arms are introduced as locals
    // initialized from sub-places of the scrutinee.
    // Lower `'label: while cond { body }`. CFG shape:
    //
    //   prev → cond_block
    //   cond_block: lower(cond); if cond then body_block else after_block
    //   body_block: lower(body); goto cond_block (back-edge)
    //   after_block: continuation
    //
    // `break` in body becomes goto(after_block); `continue` becomes
    // goto(cond_block). Loop frames stack so labelled break/continue
    // can target an outer loop.
    fn lower_while(&mut self, w: &crate::ast::WhileExpr, span: Span) {
        let cond_block = self.new_block();
        let body_block = self.new_block();
        let after_block = self.new_block();
        // Jump from current block to cond_block.
        self.set_terminator(Terminator::Goto(cond_block));

        // Build cond_block: evaluate cond, branch.
        self.current_block = cond_block;
        let cond_op = self.lower_expr_operand(&w.cond);
        self.set_terminator(Terminator::If {
            cond: cond_op,
            then_block: body_block,
            else_block: after_block,
        });

        // Build body_block. Push loop frame for break/continue.
        self.loops.push(LoopFrame {
            label: w.label.clone(),
            continue_target: cond_block,
            break_target: after_block,
        });
        self.current_block = body_block;
        self.lower_block(w.body.as_ref(), None);
        // After body, back-edge to cond_block.
        self.set_terminator(Terminator::Goto(cond_block));
        self.loops.pop();

        // Continuation.
        self.current_block = after_block;
        let _ = span;
    }

    // `for pat in iter { body }` lowers to a CFG that mirrors
    //
    //   let mut __iter = iter;
    //   loop {
    //       // (the actual `Iterator::next(&mut __iter)` call and its
    //       // Some/None destructure happen at codegen time — borrowck
    //       // doesn't model the iterator semantics; it just sees that
    //       // __iter is alive for the whole loop and that the pattern
    //       // bindings are introduced fresh each iteration.)
    //       <pattern bindings>
    //       <body>
    //       continue
    //   }
    //
    // The CFG cond_block uses a synthetic `ConstBool(true)` to keep
    // both successors (body and after) reachable in dataflow —
    // after_block then has incoming edges via that synthetic edge
    // and via any `break` in the body.
    //
    // Pattern bindings are allocated as fresh locals with `StorageLive`
    // but no `Assign`; pocket-rust's move analysis treats places
    // not present in the moved set as init-by-default, so reads of
    // the bindings in the body work without a synthetic source value.
    fn lower_for(&mut self, f: &crate::ast::ForLoop, span: Span) {
        // Move iter into __iter.
        let iter_ty = self.expr_type(f.iter.id);
        let iter_local = self.alloc_temp(iter_ty.clone(), span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(iter_local), span.copy());
        self.lower_expr_into(&f.iter, local_place(iter_local));

        let cond_block = self.new_block();
        let body_block = self.new_block();
        let after_block = self.new_block();
        self.set_terminator(Terminator::Goto(cond_block));

        // cond_block: synthetic branch. The actual exit-on-None lives
        // in codegen; borrowck just needs both successors reachable.
        self.current_block = cond_block;
        self.set_terminator(Terminator::If {
            cond: Operand {
                kind: OperandKind::ConstBool(true),
                span: span.copy(),
                node_id: None,
            },
            then_block: body_block,
            else_block: after_block,
        });

        // body_block: bind pattern (StorageLive only — init by
        // default), lower body, back-edge.
        self.loops.push(LoopFrame {
            label: f.label.clone(),
            continue_target: cond_block,
            break_target: after_block,
        });
        self.current_block = body_block;
        self.push_scope();
        self.materialize_for_loop_bindings(&f.pattern, &iter_ty);
        self.lower_block(f.body.as_ref(), None);
        self.set_terminator(Terminator::Goto(cond_block));
        self.pop_scope();
        self.loops.pop();

        self.current_block = after_block;
        let _ = span;
    }

    // Walk the for-loop's pattern, allocating a local for each
    // binding (with the binding's type derived from the iter's
    // `Iterator::Item` assoc), pushing `StorageLive` for each. No
    // `Assign` is emitted — the move analysis treats unmoved places
    // as initialized, so reads of the bindings in the body work.
    // For-loop pattern shapes are typeck-validated; complex
    // destructure patterns (struct variants etc.) are left as TODO
    // here since they need the same `Item`-source-place machinery
    // that the `if let`/`match` paths use.
    fn materialize_for_loop_bindings(&mut self, pat: &Pattern, iter_ty: &RType) {
        // Resolve `<iter_ty as Iterator>::Item` — same lookup typeck did.
        let iterator_path = vec![
            "std".to_string(),
            "iter".to_string(),
            "Iterator".to_string(),
        ];
        let item_candidates = crate::typeck::find_assoc_binding(
            self.ctx.traits,
            iter_ty,
            &iterator_path,
            "Item",
        );
        let item_ty = item_candidates
            .into_iter()
            .next()
            .expect("typeck verified Iterator impl + Item binding");
        match &pat.kind {
            crate::ast::PatternKind::Wildcard => {}
            crate::ast::PatternKind::Binding { name, name_span, by_ref, mutable } => {
                let local_ty = if *by_ref {
                    RType::Ref {
                        inner: Box::new(item_ty),
                        mutable: *mutable,
                        lifetime: LifetimeRepr::Inferred(0),
                    }
                } else {
                    item_ty
                };
                let local = self.alloc_local(
                    Some(name.clone()),
                    local_ty,
                    name_span.copy(),
                    *mutable && !*by_ref,
                    false,
                );
                self.push_stmt(CfgStmtKind::StorageLive(local), name_span.copy());
                self.bind_name(name, local);
            }
            // TODO: variant / struct / tuple / ref / at-binding /
            // or-patterns inside `for pat in iter` — needs a synthetic
            // `Item`-typed source place that borrowck's existing
            // `materialize_bindings` can destructure against. For now
            // only the bare-binding form (`for x in iter`) and
            // wildcard (`for _ in iter`) are supported by borrowck;
            // typeck accepts any pattern, so destructuring patterns
            // in for-loop position will land here as a borrowck-time
            // panic until this is filled in.
            _ => {
                let _ = item_ty;
                unimplemented!("for-loop pattern destructure not yet supported in borrowck — use `for x in iter` then destructure inside the body");
            }
        }
    }

    fn lower_break(&mut self, label: &Option<String>, span: Span) {
        let target = self.find_loop_break(label.as_deref()).expect("typeck verified break has a target");
        self.set_terminator(Terminator::Goto(target));
        // Code after a break is unreachable. Sink it into a fresh
        // block; the dataflow will ignore it (no incoming edges).
        let unreachable = self.new_block();
        self.current_block = unreachable;
        let _ = span;
    }

    fn lower_continue(&mut self, label: &Option<String>, span: Span) {
        let target = self
            .find_loop_continue(label.as_deref())
            .expect("typeck verified continue has a target");
        self.set_terminator(Terminator::Goto(target));
        let unreachable = self.new_block();
        self.current_block = unreachable;
        let _ = span;
    }

    // `return EXPR` / `return`: lower the value (if any) into the
    // function's return slot (`Local(0)` by convention) and terminate
    // with `Return`. Subsequent code in the same source block sinks
    // into a fresh unreachable block (no incoming edges).
    fn lower_return(&mut self, value: Option<&Expr>, span: Span) {
        if let Some(e) = value {
            // Pretend we're assigning to local 0 — pocket-rust's
            // existing CFG doesn't have a separate return-slot
            // concept, but the eventual `Terminator::Return` will be
            // honored regardless. We just need the move/borrow
            // checker to see that `e` was read.
            let _ = self.lower_expr_operand(e);
        }
        self.set_terminator(Terminator::Return);
        let unreachable = self.new_block();
        self.current_block = unreachable;
        let _ = span;
    }

    fn find_loop_break(&self, label: Option<&str>) -> Option<BlockId> {
        match label {
            None => self.loops.last().map(|f| f.break_target),
            Some(name) => {
                let mut i = self.loops.len();
                while i > 0 {
                    i -= 1;
                    if self.loops[i].label.as_deref() == Some(name) {
                        return Some(self.loops[i].break_target);
                    }
                }
                None
            }
        }
    }

    fn find_loop_continue(&self, label: Option<&str>) -> Option<BlockId> {
        match label {
            None => self.loops.last().map(|f| f.continue_target),
            Some(name) => {
                let mut i = self.loops.len();
                while i > 0 {
                    i -= 1;
                    if self.loops[i].label.as_deref() == Some(name) {
                        return Some(self.loops[i].continue_target);
                    }
                }
                None
            }
        }
    }

    fn lower_match_into(&mut self, m: &MatchExpr, target: Option<Place>) {
        // Evaluate scrutinee into a temp so the same place can be tested
        // and bound from across all arms.
        let scrut_ty = self.expr_type(m.scrutinee.id);
        let scrut_local = self.alloc_temp(scrut_ty.clone(), m.scrutinee.span.copy());
        self.push_stmt(
            CfgStmtKind::StorageLive(scrut_local),
            m.scrutinee.span.copy(),
        );
        self.lower_expr_into(&m.scrutinee, local_place(scrut_local));

        let merge_block = self.new_block();
        let scrut_place = local_place(scrut_local);

        let mut i = 0;
        while i < m.arms.len() {
            let arm = &m.arms[i];
            let arm_body_block = self.new_block();
            // After this arm fails, control flows to either the next
            // arm's test (if any) or to an Unreachable block (typeck
            // enforces exhaustiveness).
            let next_block = if i + 1 < m.arms.len() {
                self.new_block()
            } else {
                let u = self.new_block();
                let saved = self.current_block;
                self.current_block = u;
                self.set_terminator(Terminator::Unreachable);
                self.current_block = saved;
                u
            };
            self.lower_pattern_test(
                &arm.pattern,
                &scrut_place,
                &scrut_ty,
                arm_body_block,
                next_block,
            );
            // Build the arm body block.
            self.current_block = arm_body_block;
            self.push_scope();
            let bindings = self.collect_pattern_bindings(&arm.pattern, &scrut_place, &scrut_ty);
            self.materialize_bindings(&bindings);
            // Guard, if present.
            if let Some(g) = &arm.guard {
                let guard_op = self.lower_expr_operand(g);
                let guard_pass_block = self.new_block();
                self.set_terminator(Terminator::If {
                    cond: guard_op,
                    then_block: guard_pass_block,
                    else_block: next_block,
                });
                self.current_block = guard_pass_block;
            }
            // Lower the arm body into target.
            if let Some(t) = target.clone() {
                self.lower_expr_into(&arm.body, t);
            } else {
                let _ = self.lower_expr_operand(&arm.body);
            }
            self.set_terminator(Terminator::Goto(merge_block));
            self.pop_scope();

            self.current_block = next_block;
            i += 1;
        }

        // After all arms, the next_block of the last arm is Unreachable.
        // Continue building from merge_block.
        self.current_block = merge_block;
    }

    fn lower_if_let_into(&mut self, il: &IfLetExpr, target: Option<Place>) {
        let scrut_ty = self.expr_type(il.scrutinee.id);
        let scrut_local = self.alloc_temp(scrut_ty.clone(), il.scrutinee.span.copy());
        self.push_stmt(
            CfgStmtKind::StorageLive(scrut_local),
            il.scrutinee.span.copy(),
        );
        self.lower_expr_into(&il.scrutinee, local_place(scrut_local));

        let merge_block = self.new_block();
        let then_body_block = self.new_block();
        let else_block = self.new_block();
        let scrut_place = local_place(scrut_local);

        self.lower_pattern_test(
            &il.pattern,
            &scrut_place,
            &scrut_ty,
            then_body_block,
            else_block,
        );

        // then-branch: bind pattern, run then-block.
        self.current_block = then_body_block;
        self.push_scope();
        let bindings = self.collect_pattern_bindings(&il.pattern, &scrut_place, &scrut_ty);
        self.materialize_bindings(&bindings);
        self.lower_block(il.then_block.as_ref(), target.clone());
        self.set_terminator(Terminator::Goto(merge_block));
        self.pop_scope();

        // else-branch: just run the else-block.
        self.current_block = else_block;
        self.lower_block(il.else_block.as_ref(), target);
        self.set_terminator(Terminator::Goto(merge_block));

        self.current_block = merge_block;
    }

    // Emit CFG operations that test `pat` against `scrut_place` (whose
    // value has type `scrut_ty`). Sets the current block's terminator
    // to branch to `on_match` on success or `on_fail` on failure. After
    // returning, the current block is the test's last test block (the
    // one whose terminator was just set).
    fn lower_pattern_test(
        &mut self,
        pat: &Pattern,
        scrut_place: &Place,
        scrut_ty: &RType,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        // Match-ergonomics: typeck records on each pattern.id how many
        // `&` layers were auto-peeled before applying the pattern.
        // Replay those peels here by appending Deref projections to
        // the scrut place and stripping ref layers off the type before
        // dispatching the pattern's kind. Ref patterns peel zero
        // layers (they bind through the ref themselves).
        let pid = pat.id as usize;
        let ergo = if pid < self.ctx.pattern_ergo.len() {
            self.ctx.pattern_ergo[pid]
        } else {
            crate::typeck::PatternErgo::default()
        };
        let mut peeled_place = scrut_place.clone();
        let mut peeled_ty = scrut_ty.clone();
        let mut layer = 0u8;
        while layer < ergo.peel_layers {
            peeled_place.projections.push(Projection::Deref);
            peeled_ty = match peeled_ty {
                RType::Ref { inner, .. } => (*inner).clone(),
                _ => break, // type unexpectedly non-ref — fall through
            };
            layer += 1;
        }
        let scrut_place = &peeled_place;
        let scrut_ty = &peeled_ty;
        match &pat.kind {
            PatternKind::Wildcard
            | PatternKind::Binding { .. } => {
                self.set_terminator(Terminator::Goto(on_match));
            }
            PatternKind::At { inner, .. } => {
                self.lower_pattern_test(inner, scrut_place, scrut_ty, on_match, on_fail);
            }
            PatternKind::LitInt(n) => {
                let op = self.move_or_copy(scrut_place, scrut_ty.clone(), pat.span.copy(), None);
                self.set_terminator(Terminator::SwitchInt {
                    operand: op,
                    targets: vec![(*n, on_match)],
                    otherwise: on_fail,
                });
            }
            PatternKind::LitBool(b) => {
                let op = self.move_or_copy(scrut_place, scrut_ty.clone(), pat.span.copy(), None);
                let (then_b, else_b) = if *b {
                    (on_match, on_fail)
                } else {
                    (on_fail, on_match)
                };
                self.set_terminator(Terminator::If {
                    cond: op,
                    then_block: then_b,
                    else_block: else_b,
                });
            }
            PatternKind::Range { lo, hi } => {
                // Emit chained tests: lo <= scrut && scrut <= hi.
                // For simplicity, use a SwitchInt with every value in
                // the range as a target. Acceptable for small ranges
                // (which is the typical usage); larger ranges should
                // emit comparison ops, but pocket-rust doesn't have
                // those as Rvalues yet.
                let op = self.move_or_copy(scrut_place, scrut_ty.clone(), pat.span.copy(), None);
                let mut targets: Vec<(u64, BlockId)> = Vec::new();
                let mut v = *lo;
                while v <= *hi {
                    targets.push((v, on_match));
                    if v == u64::MAX {
                        break;
                    }
                    v += 1;
                }
                self.set_terminator(Terminator::SwitchInt {
                    operand: op,
                    targets,
                    otherwise: on_fail,
                });
            }
            PatternKind::Ref { inner, .. } => {
                let pointee_ty = match scrut_ty {
                    RType::Ref { inner, .. } => (**inner).clone(),
                    _ => unreachable!("typeck verified Ref-pattern scrutinee"),
                };
                let mut sub = scrut_place.clone();
                sub.projections.push(Projection::Deref);
                self.lower_pattern_test(inner, &sub, &pointee_ty, on_match, on_fail);
            }
            PatternKind::Tuple(elems) => {
                let elem_types: Vec<RType> = match scrut_ty {
                    RType::Tuple(es) => es.clone(),
                    _ => unreachable!("typeck verified Tuple-pattern scrutinee"),
                };
                self.test_sequence(elems, scrut_place, &elem_types, |i| {
                    Projection::TupleIndex(i as u32)
                }, on_match, on_fail);
            }
            PatternKind::VariantTuple { path, elems } => {
                self.test_variant_payload(
                    path,
                    scrut_place,
                    scrut_ty,
                    pat.span.copy(),
                    on_match,
                    on_fail,
                    |b, payload, sub_place, on_match, on_fail| {
                        let payload_types: Vec<RType> = match payload {
                            VariantPayloadResolved::Tuple(types) => types,
                            VariantPayloadResolved::Unit if elems.is_empty() => Vec::new(),
                            _ => unreachable!("typeck mismatch"),
                        };
                        b.test_sequence(elems, sub_place, &payload_types, |i| {
                            Projection::TupleIndex(i as u32)
                        }, on_match, on_fail);
                    },
                );
            }
            PatternKind::VariantStruct { path, fields, rest: _ } => {
                match scrut_ty {
                    RType::Enum {
                        path: enum_path,
                        type_args,
                        ..
                    } => {
                        // Variant struct: disc test, then field tests.
                        let variant_name = &path.segments[path.segments.len() - 1].name;
                        let variant = lookup_variant_in_enum(self.ctx.enums, enum_path, variant_name)
                            .expect("typeck verified variant exists")
                            .clone();
                        let env = enum_type_env(self.ctx.enums, enum_path, type_args);
                        let payload_fields = match &variant.payload {
                            VariantPayloadResolved::Struct(fs) => fs.clone(),
                            _ => unreachable!("typeck verified variant struct"),
                        };
                        // Disc test.
                        let disc_local =
                            self.alloc_temp(RType::Int(IntKind::I32), pat.span.copy());
                        self.push_stmt(CfgStmtKind::StorageLive(disc_local), pat.span.copy());
                        self.push_stmt(
                            CfgStmtKind::Assign {
                                place: local_place(disc_local),
                                rvalue: Rvalue::Discriminant(scrut_place.clone()),
                            },
                            pat.span.copy(),
                        );
                        let payload_match_block = self.new_block();
                        self.set_terminator(Terminator::SwitchInt {
                            operand: Operand {
                                kind: OperandKind::Move(local_place(disc_local)),
                                span: pat.span.copy(),
                                node_id: None,
                            },
                            targets: vec![(variant.disc as u64, payload_match_block)],
                            otherwise: on_fail,
                        });
                        self.current_block = payload_match_block;
                        self.test_struct_fields(
                            fields,
                            scrut_place,
                            &payload_fields,
                            &env,
                            on_match,
                            on_fail,
                        );
                    }
                    RType::Struct {
                        path: spath,
                        type_args,
                        ..
                    } => {
                        // Plain struct destructure: no disc test, just
                        // field tests.
                        let entry = crate::typeck::struct_lookup(self.ctx.structs, spath)
                            .expect("typeck verified struct");
                        let payload_fields: Vec<crate::typeck::RTypedField> = entry
                            .fields
                            .iter()
                            .map(|f| crate::typeck::RTypedField {
                                name: f.name.clone(),
                                name_span: f.name_span.copy(),
                                ty: f.ty.clone(),
                                vis: f.vis.clone(),
                            })
                            .collect();
                        let mut env: Vec<(String, RType)> = Vec::new();
                        let mut k = 0;
                        while k < entry.type_params.len() && k < type_args.len() {
                            env.push((entry.type_params[k].clone(), type_args[k].clone()));
                            k += 1;
                        }
                        self.test_struct_fields(
                            fields,
                            scrut_place,
                            &payload_fields,
                            &env,
                            on_match,
                            on_fail,
                        );
                    }
                    _ => unreachable!("typeck verified VariantStruct scrutinee"),
                }
            }
            PatternKind::Or(alts) => {
                let mut idx = 0;
                while idx < alts.len() {
                    let next = if idx + 1 < alts.len() {
                        self.new_block()
                    } else {
                        on_fail
                    };
                    self.lower_pattern_test(
                        &alts[idx],
                        scrut_place,
                        scrut_ty,
                        on_match,
                        next,
                    );
                    self.current_block = next;
                    idx += 1;
                }
            }
        }
    }

    // Sequential tests over named struct/variant-struct fields. Each
    // field's type comes from the declared payload after substituting
    // the enum/struct type-arg env.
    fn test_struct_fields(
        &mut self,
        fields: &Vec<ast::FieldPattern>,
        scrut_place: &Place,
        payload_fields: &Vec<crate::typeck::RTypedField>,
        env: &Vec<(String, RType)>,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        if fields.is_empty() {
            self.set_terminator(Terminator::Goto(on_match));
            return;
        }
        let mut idx = 0;
        while idx < fields.len() {
            let next = if idx + 1 < fields.len() {
                self.new_block()
            } else {
                on_match
            };
            let fname = &fields[idx].name;
            let fty = payload_fields
                .iter()
                .find(|f| f.name == *fname)
                .map(|f| substitute_rtype(&f.ty, env))
                .expect("typeck verified field");
            let mut sub = scrut_place.clone();
            sub.projections.push(Projection::Field(fname.clone()));
            self.lower_pattern_test(&fields[idx].pattern, &sub, &fty, next, on_fail);
            self.current_block = next;
            idx += 1;
        }
    }

    // Helper for sequential pattern tests over a positional list of
    // sub-patterns (tuples and tuple variants). Each test branches to
    // the next test on success or to `on_fail` on failure; the last
    // success branch goes to `on_match`.
    fn test_sequence(
        &mut self,
        pats: &Vec<Pattern>,
        scrut_place: &Place,
        elem_tys: &Vec<RType>,
        proj: impl Fn(usize) -> Projection,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        if pats.is_empty() {
            self.set_terminator(Terminator::Goto(on_match));
            return;
        }
        let mut idx = 0;
        while idx < pats.len() {
            let next = if idx + 1 < pats.len() {
                self.new_block()
            } else {
                on_match
            };
            let mut sub = scrut_place.clone();
            sub.projections.push(proj(idx));
            self.lower_pattern_test(&pats[idx], &sub, &elem_tys[idx], next, on_fail);
            self.current_block = next;
            idx += 1;
        }
    }

    // Helper for variant patterns: emit the discriminant test, then
    // hand the payload sub-place + variant payload metadata to a
    // callback that recurses on the payload pattern. The callback runs
    // in the `payload_match_block`, which is reached only if the
    // discriminant matches.
    fn test_variant_payload<F>(
        &mut self,
        path: &ast::Path,
        scrut_place: &Place,
        scrut_ty: &RType,
        span: Span,
        on_match: BlockId,
        on_fail: BlockId,
        recurse: F,
    ) where
        F: FnOnce(&mut Self, VariantPayloadResolved, &Place, BlockId, BlockId),
    {
        // Resolve variant from path against the scrut's enum type.
        let (enum_path_canonical, type_args) = match scrut_ty {
            RType::Enum {
                path,
                type_args,
                ..
            } => (path.clone(), type_args.clone()),
            // Handle plain struct (not enum) — fall through to the
            // recurse step with no disc test.
            RType::Struct { .. } => {
                recurse(
                    self,
                    VariantPayloadResolved::Unit,
                    scrut_place,
                    on_match,
                    on_fail,
                );
                return;
            }
            _ => unreachable!("typeck verified variant pattern scrutinee"),
        };

        // Find the matching variant by name within the scrutinee's
        // canonical enum.
        let variant_name = &path.segments[path.segments.len() - 1].name;
        let variant = lookup_variant_in_enum(self.ctx.enums, &enum_path_canonical, variant_name)
            .expect("typeck verified variant exists");
        let env = enum_type_env(self.ctx.enums, &enum_path_canonical, &type_args);
        let disc = variant.disc;
        let variant = variant.clone();
        let payload = substitute_variant_payload(&variant.payload, &env);

        // Read discriminant.
        let disc_local = self.alloc_temp(RType::Int(IntKind::I32), span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(disc_local), span.copy());
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(disc_local),
                rvalue: Rvalue::Discriminant(scrut_place.clone()),
            },
            span.copy(),
        );
        let payload_match_block = self.new_block();
        self.set_terminator(Terminator::SwitchInt {
            operand: Operand {
                kind: OperandKind::Move(local_place(disc_local)),
                span: span.copy(),
                node_id: None,
            },
            targets: vec![(disc as u64, payload_match_block)],
            otherwise: on_fail,
        });
        self.current_block = payload_match_block;
        recurse(self, payload, scrut_place, on_match, on_fail);
    }

    // ---------- Pattern bindings ----------

    // Walk a pattern, collecting bindings: each (name, sub-place,
    // by_ref, mutable, type). The sub-place is the path from
    // `scrut_place` to the pattern position.
    fn collect_pattern_bindings(
        &mut self,
        pat: &Pattern,
        scrut_place: &Place,
        scrut_ty: &RType,
    ) -> Vec<PatternBinding> {
        let mut out: Vec<PatternBinding> = Vec::new();
        self.collect_bindings_into(pat, scrut_place, scrut_ty, &mut out);
        out
    }

    fn collect_bindings_into(
        &mut self,
        pat: &Pattern,
        scrut_place: &Place,
        scrut_ty: &RType,
        out: &mut Vec<PatternBinding>,
    ) {
        // Apply match-ergonomics peels. Same logic as
        // `lower_pattern_test`: append Deref projections to the place
        // and strip ref layers off the type for each peel layer that
        // typeck recorded on this pattern.id.
        let pid = pat.id as usize;
        let ergo = if pid < self.ctx.pattern_ergo.len() {
            self.ctx.pattern_ergo[pid]
        } else {
            crate::typeck::PatternErgo::default()
        };
        let mut peeled_place = scrut_place.clone();
        let mut peeled_ty = scrut_ty.clone();
        let mut layer = 0u8;
        while layer < ergo.peel_layers {
            peeled_place.projections.push(Projection::Deref);
            peeled_ty = match peeled_ty {
                RType::Ref { inner, .. } => (*inner).clone(),
                _ => break,
            };
            layer += 1;
        }
        let scrut_place = &peeled_place;
        let scrut_ty = &peeled_ty;
        match &pat.kind {
            PatternKind::Wildcard
            | PatternKind::LitInt(_)
            | PatternKind::LitBool(_)
            | PatternKind::Range { .. } => {}
            PatternKind::Binding {
                name,
                name_span,
                by_ref,
                mutable,
            } => {
                // Match-ergonomics override (typeck-recorded) takes
                // precedence over AST `by_ref`/`mutable`. A non-Move
                // default binding mode within an auto-peeled scope
                // turns the binding into `&T` / `&mut T` even though
                // the user wrote a plain ident.
                let (eff_by_ref, eff_mutable) = if ergo.binding_override_ref {
                    (true, ergo.binding_mutable_ref)
                } else {
                    (*by_ref, *mutable)
                };
                out.push(PatternBinding {
                    name: name.clone(),
                    place: scrut_place.clone(),
                    ty: scrut_ty.clone(),
                    by_ref: eff_by_ref,
                    mutable: eff_mutable,
                    span: name_span.copy(),
                });
            }
            PatternKind::At {
                name,
                name_span,
                inner,
            } => {
                out.push(PatternBinding {
                    name: name.clone(),
                    place: scrut_place.clone(),
                    ty: scrut_ty.clone(),
                    by_ref: false,
                    mutable: false,
                    span: name_span.copy(),
                });
                self.collect_bindings_into(inner, scrut_place, scrut_ty, out);
            }
            PatternKind::Ref { inner, .. } => {
                let pointee_ty = match scrut_ty {
                    RType::Ref { inner, .. } => (**inner).clone(),
                    _ => return,
                };
                let mut sub = scrut_place.clone();
                sub.projections.push(Projection::Deref);
                self.collect_bindings_into(inner, &sub, &pointee_ty, out);
            }
            PatternKind::Tuple(elems) => {
                let elem_tys: Vec<RType> = match scrut_ty {
                    RType::Tuple(es) => es.clone(),
                    _ => return,
                };
                let mut i = 0;
                while i < elems.len() && i < elem_tys.len() {
                    let mut sub = scrut_place.clone();
                    sub.projections.push(Projection::TupleIndex(i as u32));
                    self.collect_bindings_into(&elems[i], &sub, &elem_tys[i], out);
                    i += 1;
                }
            }
            PatternKind::VariantTuple { path, elems } => {
                if let RType::Enum {
                    path: enum_path,
                    type_args,
                    ..
                } = scrut_ty
                {
                    let variant_name = &path.segments[path.segments.len() - 1].name;
                    let env = enum_type_env(self.ctx.enums, enum_path, type_args);
                    if let Some(variant) =
                        lookup_variant_in_enum(self.ctx.enums, enum_path, variant_name)
                    {
                        if let VariantPayloadResolved::Tuple(types) = &variant.payload {
                            let mut i = 0;
                            while i < elems.len() && i < types.len() {
                                let ty = substitute_rtype(&types[i], &env);
                                let mut sub = scrut_place.clone();
                                sub.projections.push(Projection::TupleIndex(i as u32));
                                self.collect_bindings_into(&elems[i], &sub, &ty, out);
                                i += 1;
                            }
                        }
                    }
                }
            }
            PatternKind::VariantStruct {
                path,
                fields,
                rest: _,
            } => {
                let (payload_fields, env) = match scrut_ty {
                    RType::Enum {
                        path: enum_path,
                        type_args,
                        ..
                    } => {
                        let variant_name = &path.segments[path.segments.len() - 1].name;
                        let env = enum_type_env(self.ctx.enums, enum_path, type_args);
                        let pf = lookup_variant_in_enum(self.ctx.enums, enum_path, variant_name)
                            .and_then(|v| match &v.payload {
                                VariantPayloadResolved::Struct(fs) => Some(fs.clone()),
                                _ => None,
                            });
                        (pf, env)
                    }
                    RType::Struct {
                        path: spath,
                        type_args,
                        ..
                    } => {
                        let pf = crate::typeck::struct_lookup(self.ctx.structs, spath).map(|s| {
                            let mut env: Vec<(String, RType)> = Vec::new();
                            let mut k = 0;
                            while k < s.type_params.len() && k < type_args.len() {
                                env.push((s.type_params[k].clone(), type_args[k].clone()));
                                k += 1;
                            }
                            (
                                s.fields
                                    .iter()
                                    .map(|f| crate::typeck::RTypedField {
                                        name: f.name.clone(),
                                        name_span: f.name_span.copy(),
                                        ty: f.ty.clone(),
                                        vis: f.vis.clone(),
                                    })
                                    .collect::<Vec<_>>(),
                                env,
                            )
                        });
                        match pf {
                            Some((fs, env)) => (Some(fs), env),
                            None => (None, Vec::<(String, RType)>::new()),
                        }
                    }
                    _ => (None, Vec::<(String, RType)>::new()),
                };
                if let Some(payload_fields) = payload_fields {
                    let mut i = 0;
                    while i < fields.len() {
                        let fname = &fields[i].name;
                        let fty = payload_fields
                            .iter()
                            .find(|f| f.name == *fname)
                            .map(|f| substitute_rtype(&f.ty, &env));
                        if let Some(fty) = fty {
                            let mut sub = scrut_place.clone();
                            sub.projections.push(Projection::Field(fname.clone()));
                            self.collect_bindings_into(&fields[i].pattern, &sub, &fty, out);
                        }
                        i += 1;
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Or-patterns must bind the same names on every alt;
                // typeck enforced this. Walk only the first alt — its
                // binding shape is canonical.
                if !alts.is_empty() {
                    self.collect_bindings_into(&alts[0], scrut_place, scrut_ty, out);
                }
            }
        }
    }

    // Materialize the bindings: alloc a local for each, emit an
    // initializing Assign (move/copy or borrow), and bind the name in
    // the current scope.
    fn materialize_bindings(&mut self, bindings: &[PatternBinding]) {
        let mut i = 0;
        while i < bindings.len() {
            let b = &bindings[i];
            let local_ty = if b.by_ref {
                RType::Ref {
                    inner: Box::new(b.ty.clone()),
                    mutable: b.mutable,
                    lifetime: LifetimeRepr::Inferred(0),
                }
            } else {
                b.ty.clone()
            };
            let local = self.alloc_local(
                Some(b.name.clone()),
                local_ty,
                b.span.copy(),
                b.mutable && !b.by_ref,
                false,
            );
            self.push_stmt(CfgStmtKind::StorageLive(local), b.span.copy());
            let rvalue = if b.by_ref {
                let region = self.fresh_region();
                Rvalue::Borrow {
                    mutable: b.mutable,
                    place: b.place.clone(),
                    region,
                }
            } else {
                Rvalue::Use(self.move_or_copy(&b.place, b.ty.clone(), b.span.copy(), None))
            };
            self.push_stmt(
                CfgStmtKind::Assign {
                    place: local_place(local),
                    rvalue,
                },
                b.span.copy(),
            );
            self.bind_name(&b.name, local);
            i += 1;
        }
    }
}

struct PatternBinding {
    name: String,
    place: Place,
    ty: RType,
    by_ref: bool,
    mutable: bool,
    span: Span,
}

// Find a variant within a known enum (by canonical enum path) by name.
// Returns the variant entry. Used after the scrutinee's enum is known
// from its RType; avoids reaching into use-scope/reexport machinery.
fn lookup_variant_in_enum<'a>(
    enums: &'a EnumTable,
    enum_path: &Vec<String>,
    variant_name: &str,
) -> Option<&'a EnumVariantEntry> {
    let mut i = 0;
    while i < enums.entries.len() {
        if enums.entries[i].path == *enum_path {
            let mut j = 0;
            while j < enums.entries[i].variants.len() {
                if enums.entries[i].variants[j].name == variant_name {
                    return Some(&enums.entries[i].variants[j]);
                }
                j += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

// Build a substitution env from the scrutinee's enum type-args + the
// enum's type-param names.
fn enum_type_env(enums: &EnumTable, enum_path: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < enums.entries.len() {
        if enums.entries[i].path == *enum_path {
            let mut k = 0;
            while k < enums.entries[i].type_params.len() && k < type_args.len() {
                env.push((
                    enums.entries[i].type_params[k].clone(),
                    type_args[k].clone(),
                ));
                k += 1;
            }
            return env;
        }
        i += 1;
    }
    env
}

fn substitute_variant_payload(
    payload: &VariantPayloadResolved,
    env: &Vec<(String, RType)>,
) -> VariantPayloadResolved {
    match payload {
        VariantPayloadResolved::Unit => VariantPayloadResolved::Unit,
        VariantPayloadResolved::Tuple(types) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < types.len() {
                out.push(substitute_rtype(&types[i], env));
                i += 1;
            }
            VariantPayloadResolved::Tuple(out)
        }
        VariantPayloadResolved::Struct(fields) => {
            let mut out: Vec<crate::typeck::RTypedField> = Vec::new();
            let mut i = 0;
            while i < fields.len() {
                out.push(crate::typeck::RTypedField {
                    name: fields[i].name.clone(),
                    name_span: fields[i].name_span.copy(),
                    ty: substitute_rtype(&fields[i].ty, env),
                    vis: fields[i].vis.clone(),
                });
                i += 1;
            }
            VariantPayloadResolved::Struct(out)
        }
    }
}

fn local_place(local: LocalId) -> Place {
    Place {
        root: local,
        projections: Vec::new(),
    }
}

fn unit_rtype() -> RType {
    RType::Tuple(Vec::new())
}

fn is_unit(rt: &RType) -> bool {
    matches!(rt, RType::Tuple(elems) if elems.is_empty())
}

