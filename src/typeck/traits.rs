use super::{
    FuncTable, InferType, RType, Subst,
    TraitTable, rtype_eq, substitute_rtype, trait_lookup,
};
use crate::span::Span;

// Result of solving `(trait, concrete_type)` against the impl table:
// the impl row's index plus the substitution from the impl's type-params
// to the concrete pieces of the type that filled them.
pub struct ImplResolution {
    pub impl_idx: usize,
    pub subst: Vec<(String, RType)>,
}

// Recursive impl resolver. Given a (trait, concrete_type) query, find an
// impl row whose target pattern matches and whose `where T: Bound`
// constraints all recursively resolve. Depth-bounded to prevent runaway
// recursion via pathological self-referential impls.
pub fn solve_impl(
    trait_path: &Vec<String>,
    concrete: &RType,
    traits: &TraitTable,
    depth: u32,
) -> Option<ImplResolution> {
    solve_impl_in_ctx(trait_path, concrete, traits, &Vec::new(), &Vec::new(), depth)
}

// Like `solve_impl`, but also recognizes a `Param(name)` concrete as
// satisfying `trait_path` when one of the param's in-scope bounds (or
// any of that bound's transitive supertraits) equals `trait_path`. The
// `type_params`/`type_param_bounds` slices align in length and order;
// they're threaded through the recursive impl-bound check so nested
// generic obligations like `impl<T: PartialEq> Eq for Wrap<T>` (which
// recurses to `Wrap<T>: PartialEq` → `T: PartialEq`) resolve via the
// outer context.
pub fn solve_impl_in_ctx(
    trait_path: &Vec<String>,
    concrete: &RType,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bounds: &Vec<Vec<Vec<String>>>,
    depth: u32,
) -> Option<ImplResolution> {
    if depth > 32 {
        return None;
    }
    if let RType::Param(name) = concrete {
        let mut i = 0;
        while i < type_params.len() {
            if type_params[i] == *name && i < type_param_bounds.len() {
                let mut b = 0;
                while b < type_param_bounds[i].len() {
                    let closure = supertrait_closure(&type_param_bounds[i][b], traits);
                    let mut k = 0;
                    while k < closure.len() {
                        if &closure[k] == trait_path {
                            return Some(ImplResolution {
                                impl_idx: usize::MAX,
                                subst: Vec::new(),
                            });
                        }
                        k += 1;
                    }
                    b += 1;
                }
                return None;
            }
            i += 1;
        }
        return None;
    }
    let mut i = 0;
    while i < traits.impls.len() {
        let row = &traits.impls[i];
        if &row.trait_path != trait_path {
            i += 1;
            continue;
        }
        let mut subst: Vec<(String, RType)> = Vec::new();
        if !try_match_rtype(&row.target, concrete, &mut subst) {
            i += 1;
            continue;
        }
        let mut all_bounds_ok = true;
        let mut p = 0;
        while p < row.impl_type_params.len() {
            let name = &row.impl_type_params[p];
            let mut bound_concrete: Option<RType> = None;
            let mut k = 0;
            while k < subst.len() {
                if subst[k].0 == *name {
                    bound_concrete = Some(subst[k].1.clone());
                    break;
                }
                k += 1;
            }
            if let Some(tc) = bound_concrete {
                let mut b = 0;
                while b < row.impl_type_param_bounds[p].len() {
                    let bound_trait = &row.impl_type_param_bounds[p][b];
                    if solve_impl_in_ctx(
                        bound_trait,
                        &tc,
                        traits,
                        type_params,
                        type_param_bounds,
                        depth + 1,
                    )
                    .is_none()
                    {
                        all_bounds_ok = false;
                        break;
                    }
                    b += 1;
                }
            }
            if !all_bounds_ok {
                break;
            }
            p += 1;
        }
        if all_bounds_ok {
            return Some(ImplResolution {
                impl_idx: i,
                subst,
            });
        }
        i += 1;
    }
    None
}

// Resolve `<base as Trait>::Name` (or `<base as ?>::Name` when
// `trait_path` is empty) by scanning all registered impl rows. Returns
// every concrete binding the lookup matches — empty Vec when no impl
// covers `base+name`, length > 1 when multiple traits supply the same
// assoc name on the same target (caller decides ambiguity).
pub fn find_assoc_binding(
    traits: &TraitTable,
    base: &RType,
    trait_path: &Vec<String>,
    name: &str,
) -> Vec<RType> {
    let mut results: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < traits.impls.len() {
        let row = &traits.impls[i];
        if !trait_path.is_empty() && &row.trait_path != trait_path {
            i += 1;
            continue;
        }
        let trait_entry = match trait_lookup(traits, &row.trait_path) {
            Some(e) => e,
            None => {
                i += 1;
                continue;
            }
        };
        if !trait_entry.assoc_types.iter().any(|a| a == name) {
            i += 1;
            continue;
        }
        let mut subst: Vec<(String, RType)> = Vec::new();
        if !try_match_rtype(&row.target, base, &mut subst) {
            i += 1;
            continue;
        }
        let mut k = 0;
        while k < row.assoc_type_bindings.len() {
            if row.assoc_type_bindings[k].0 == name {
                let concrete = substitute_rtype(&row.assoc_type_bindings[k].1, &subst);
                results.push(concrete);
                break;
            }
            k += 1;
        }
        i += 1;
    }
    results
}

// Walk an RType, replacing every `AssocProj` whose base is concrete
// enough to find a unique impl binding via `find_assoc_binding`. Leaves
// unresolved projections as-is (e.g. when the base is a Param with no
// matching bound, or when no impl covers it yet). Recursive — handles
// nested `T::Item::Inner`.
pub fn concretize_assoc_proj(rt: &RType, traits: &TraitTable) -> RType {
    concretize_assoc_proj_with_bounds(rt, traits, &Vec::new(), &Vec::new())
}

// Like `concretize_assoc_proj` but also resolves `T::Name` projections
// against the in-scope type-param bounds. `type_param_bound_assoc[i]`
// is the list of (assoc_name, concrete_type) constraints attached to
// `type_params[i]`'s bounds (from `Trait<Name = X>` syntax). When the
// projection's base is a `Param("T")` whose bound carries a matching
// constraint, the projection resolves to the constrained type.
pub fn concretize_assoc_proj_with_bounds(
    rt: &RType,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bound_assoc: &Vec<Vec<(String, RType)>>,
) -> RType {
    let recurse = |inner: &RType| {
        concretize_assoc_proj_with_bounds(inner, traits, type_params, type_param_bound_assoc)
    };
    match rt {
        RType::AssocProj { base, trait_path, name } => {
            let new_base = recurse(base);
            // T::Name via in-scope bound constraint?
            if let RType::Param(t_name) = &new_base {
                let mut i = 0;
                while i < type_params.len() {
                    if &type_params[i] == t_name && i < type_param_bound_assoc.len() {
                        let mut k = 0;
                        while k < type_param_bound_assoc[i].len() {
                            if &type_param_bound_assoc[i][k].0 == name {
                                return concretize_assoc_proj_with_bounds(
                                    &type_param_bound_assoc[i][k].1,
                                    traits,
                                    type_params,
                                    type_param_bound_assoc,
                                );
                            }
                            k += 1;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            // Otherwise fall through to traits-table lookup.
            let candidates = find_assoc_binding(traits, &new_base, trait_path, name);
            if candidates.len() == 1 {
                concretize_assoc_proj_with_bounds(
                    &candidates[0],
                    traits,
                    type_params,
                    type_param_bound_assoc,
                )
            } else {
                RType::AssocProj {
                    base: Box::new(new_base),
                    trait_path: trait_path.clone(),
                    name: name.clone(),
                }
            }
        }
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(recurse(inner)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(recurse(inner)),
            mutable: *mutable,
        },
        RType::Struct { path, type_args, lifetime_args } => {
            let mut new_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                new_args.push(recurse(&type_args[i]));
                i += 1;
            }
            RType::Struct {
                path: path.clone(),
                type_args: new_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let mut new_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                new_args.push(recurse(&type_args[i]));
                i += 1;
            }
            RType::Enum {
                path: path.clone(),
                type_args: new_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        RType::Tuple(elems) => {
            let mut new_elems: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                new_elems.push(recurse(&elems[i]));
                i += 1;
            }
            RType::Tuple(new_elems)
        }
        RType::Slice(inner) => RType::Slice(Box::new(recurse(inner))),
        _ => rt.clone(),
    }
}

// Find a method by name within a registered trait impl. Returns the
// FuncTable position so the caller can dispatch / monomorphize.
pub fn find_trait_impl_method(
    funcs: &FuncTable,
    impl_idx: usize,
    method_name: &str,
) -> Option<MethodCandidate> {
    let mut i = 0;
    while i < funcs.entries.len() {
        if funcs.entries[i].trait_impl_idx == Some(impl_idx)
            && !funcs.entries[i].path.is_empty()
            && funcs.entries[i].path[funcs.entries[i].path.len() - 1] == method_name
        {
            return Some(MethodCandidate::Direct(i));
        }
        i += 1;
    }
    let mut i = 0;
    while i < funcs.templates.len() {
        if funcs.templates[i].trait_impl_idx == Some(impl_idx)
            && !funcs.templates[i].path.is_empty()
            && funcs.templates[i].path[funcs.templates[i].path.len() - 1] == method_name
        {
            return Some(MethodCandidate::Template(i));
        }
        i += 1;
    }
    None
}

// One method-resolution candidate: either a non-generic concrete method
// (`Direct(idx)` indexes into `FuncTable.entries`) or a generic-method
// template (`Template(idx)` indexes into `FuncTable.templates`).
#[derive(Clone, Copy)]
pub enum MethodCandidate {
    Direct(usize),
    Template(usize),
}

// Walks the FuncTable for every method-shaped entry/template whose name
// (last path segment) matches `method_name`. T2.6: no longer filters by
// the impl_target's outermost struct path — that previously hid
// blanket impls like `impl<T> Show for &T` or `impl Show for u32` whose
// targets aren't structs. The caller runs `try_match_against_infer`
// against each candidate's `impl_target` to filter to those that
// actually match the receiver type.
pub fn find_method_candidates(
    funcs: &FuncTable,
    method_name: &str,
) -> Vec<MethodCandidate> {
    let mut out: Vec<MethodCandidate> = Vec::new();
    let mut i = 0;
    while i < funcs.entries.len() {
        if funcs.entries[i].impl_target.is_some()
            && !funcs.entries[i].path.is_empty()
            && funcs.entries[i].path[funcs.entries[i].path.len() - 1] == method_name
        {
            out.push(MethodCandidate::Direct(i));
        }
        i += 1;
    }
    let mut i = 0;
    while i < funcs.templates.len() {
        if funcs.templates[i].impl_target.is_some()
            && !funcs.templates[i].path.is_empty()
            && funcs.templates[i].path[funcs.templates[i].path.len() - 1] == method_name
        {
            out.push(MethodCandidate::Template(i));
        }
        i += 1;
    }
    out
}

// Structural pattern matcher for impl-target lookup. Walks `pattern` and
// `concrete` in lockstep; whenever `pattern` reaches `RType::Param(name)`,
// either binds `name` in `subst` or, if already bound, requires equality
// with the existing binding (so `impl<T> Pair<T, T>` only matches
// `Pair<X, X>` for some X). Returns true on success and leaves new
// bindings in `subst`; returns false on shape mismatch (subst may be
// partially mutated — caller should snapshot/restore if it cares).
//
// Lifetime handling: pattern `Named(impl_lt)` matches any concrete
// lifetime (lifetimes in patterns aren't tracked through the subst yet —
// follow-up). Pattern `Inferred(_)` likewise matches anything. Concrete
// lifetimes only need to match shape-wise, not by id.
pub fn try_match_rtype(
    pattern: &RType,
    concrete: &RType,
    subst: &mut Vec<(String, RType)>,
) -> bool {
    try_match_rtype_ctx(pattern, concrete, subst, true)
}

// `param_must_be_sized` carries the implicit `T: Sized` rule: at the
// outer pattern position (impl_target itself, or inside Tuple/Struct/
// Enum elements), a Param binding must be Sized. Recursing through
// `Ref`/`RawPtr` flips it off — `&T` and `*const T` accept unsized
// `T` (which is what makes `impl<T> Copy for &T` cover `&str`, mirror
// of Rust's `impl<T: ?Sized> Copy for &T`).
fn try_match_rtype_ctx(
    pattern: &RType,
    concrete: &RType,
    subst: &mut Vec<(String, RType)>,
    param_must_be_sized: bool,
) -> bool {
    match (pattern, concrete) {
        (RType::Param(name), c) => {
            if param_must_be_sized && !crate::typeck::is_sized(c) {
                return false;
            }
            // Already bound? Must equal.
            let mut i = 0;
            while i < subst.len() {
                if subst[i].0 == *name {
                    return rtype_eq(&subst[i].1, c);
                }
                i += 1;
            }
            subst.push((name.clone(), c.clone()));
            true
        }
        (RType::Int(ka), RType::Int(kb)) => ka == kb,
        (
            RType::Struct {
                path: pa,
                type_args: aa,
                ..
            },
            RType::Struct {
                path: pb,
                type_args: ab,
                ..
            },
        ) => {
            if pa != pb || aa.len() != ab.len() {
                return false;
            }
            let mut i = 0;
            while i < aa.len() {
                if !try_match_rtype_ctx(&aa[i], &ab[i], subst, true) {
                    return false;
                }
                i += 1;
            }
            true
        }
        (
            RType::Ref {
                inner: ia,
                mutable: ma,
                ..
            },
            RType::Ref {
                inner: ib,
                mutable: mb,
                ..
            },
        ) => ma == mb && try_match_rtype_ctx(ia, ib, subst, false),
        (
            RType::RawPtr {
                inner: ia,
                mutable: ma,
            },
            RType::RawPtr {
                inner: ib,
                mutable: mb,
            },
        ) => ma == mb && try_match_rtype_ctx(ia, ib, subst, false),
        (RType::Bool, RType::Bool) => true,
        (RType::Never, RType::Never) => true,
        (RType::Char, RType::Char) => true,
        (RType::Tuple(a), RType::Tuple(b)) => {
            if a.len() != b.len() {
                return false;
            }
            let mut i = 0;
            while i < a.len() {
                if !try_match_rtype_ctx(&a[i], &b[i], subst, true) {
                    return false;
                }
                i += 1;
            }
            true
        }
        (
            RType::Enum {
                path: pa,
                type_args: aa,
                ..
            },
            RType::Enum {
                path: pb,
                type_args: ab,
                ..
            },
        ) => {
            if pa != pb || aa.len() != ab.len() {
                return false;
            }
            let mut i = 0;
            while i < aa.len() {
                if !try_match_rtype_ctx(&aa[i], &ab[i], subst, true) {
                    return false;
                }
                i += 1;
            }
            true
        }
        _ => false,
    }
}

// InferType-flavored variant of `try_match_rtype`: matches an `RType`
// pattern against an `InferType` concrete value, after substituting the
// concrete through `subst` to resolve any bound vars. Repeat-param cases
// (`impl<T> Pair<T, T>` matched against `Pair<?v, ?w>`) accumulate
// pending unifications for the caller to commit if this candidate wins.
pub(crate) fn try_match_against_infer(
    pattern: &RType,
    concrete: &InferType,
    subst: &Subst,
    env: &mut Vec<(String, InferType)>,
    pending: &mut Vec<(InferType, InferType)>,
) -> bool {
    try_match_against_infer_ctx(pattern, concrete, subst, env, pending, true)
}

// Like `try_match_against_infer`, with the same `param_must_be_sized`
// context as `try_match_rtype_ctx`. Recursing into Ref/RawPtr flips it
// off so DST-bearing refs (`&str`, `&[U]`) match impls like
// `impl<T> Copy for &T`.
fn try_match_against_infer_ctx(
    pattern: &RType,
    concrete: &InferType,
    subst: &Subst,
    env: &mut Vec<(String, InferType)>,
    pending: &mut Vec<(InferType, InferType)>,
    param_must_be_sized: bool,
) -> bool {
    let resolved = subst.substitute(concrete);
    match pattern {
        RType::Param(name) => {
            if param_must_be_sized && !crate::typeck::is_sized_infer(&resolved) {
                return false;
            }
            // Already bound? Stage a unification with the prior binding.
            let mut existing: Option<InferType> = None;
            let mut k = 0;
            while k < env.len() {
                if env[k].0 == *name {
                    existing = Some(env[k].1.clone());
                    break;
                }
                k += 1;
            }
            match existing {
                Some(prior) => {
                    pending.push((prior, resolved));
                    true
                }
                None => {
                    env.push((name.clone(), resolved));
                    true
                }
            }
        }
        RType::Int(ka) => match &resolved {
            InferType::Int(kb) => ka == kb,
            _ => false,
        },
        RType::Bool => matches!(resolved, InferType::Bool),
        RType::Struct {
            path: pa,
            type_args: aa,
            ..
        } => match &resolved {
            InferType::Struct {
                path: pb,
                type_args: ab,
                ..
            } => {
                if pa != pb || aa.len() != ab.len() {
                    return false;
                }
                let mut i = 0;
                while i < aa.len() {
                    if !try_match_against_infer_ctx(&aa[i], &ab[i], subst, env, pending, true) {
                        return false;
                    }
                    i += 1;
                }
                true
            }
            _ => false,
        },
        RType::Ref {
            inner: ia,
            mutable: ma,
            ..
        } => match &resolved {
            InferType::Ref {
                inner: ib,
                mutable: mb,
                ..
            } => ma == mb && try_match_against_infer_ctx(ia, ib, subst, env, pending, false),
            _ => false,
        },
        RType::RawPtr {
            inner: ia,
            mutable: ma,
        } => match &resolved {
            InferType::RawPtr {
                inner: ib,
                mutable: mb,
            } => ma == mb && try_match_against_infer_ctx(ia, ib, subst, env, pending, false),
            _ => false,
        },
        RType::Tuple(pa) => match &resolved {
            InferType::Tuple(pb) => {
                if pa.len() != pb.len() {
                    return false;
                }
                let mut i = 0;
                while i < pa.len() {
                    if !try_match_against_infer_ctx(&pa[i], &pb[i], subst, env, pending, true) {
                        return false;
                    }
                    i += 1;
                }
                true
            }
            _ => false,
        },
        RType::Enum {
            path: pa,
            type_args: aa,
            ..
        } => match &resolved {
            InferType::Enum {
                path: pb,
                type_args: ab,
                ..
            } => {
                if pa != pb || aa.len() != ab.len() {
                    return false;
                }
                let mut i = 0;
                while i < aa.len() {
                    if !try_match_against_infer_ctx(&aa[i], &ab[i], subst, env, pending, true) {
                        return false;
                    }
                    i += 1;
                }
                true
            }
            _ => false,
        },
        RType::Slice(ia) => match &resolved {
            InferType::Slice(ib) => try_match_against_infer_ctx(ia, ib, subst, env, pending, true),
            _ => false,
        },
        RType::Str => matches!(resolved, InferType::Str),
        // An unconcretized projection in the pattern can't match
        // anything structurally; the candidate would need its
        // projection resolved before reaching dispatch. Conservative
        // false — caller eagerly concretizes or rejects upstream.
        RType::AssocProj { .. } => false,
        // An impl pattern of `!` only matches `!` (no inhabitants
        // means there's nothing else `!` should be picked up by).
        RType::Never => matches!(resolved, InferType::Never),
        RType::Char => matches!(resolved, InferType::Char),
    }
}
pub fn find_trait_impl_idx_by_span(
    table: &TraitTable,
    file: &str,
    span: &Span,
) -> Option<usize> {
    let mut i = 0;
    while i < table.impls.len() {
        let row = &table.impls[i];
        if row.file == file
            && row.span.start.line == span.start.line
            && row.span.start.col == span.start.col
        {
            return Some(i);
        }
        i += 1;
    }
    None
}


// Returns `start` plus every transitive supertrait, deduplicated. Cycles
// are broken by the dedup check (a trait already in `out` is not pushed
// or recursed into again), so a malformed `trait A: B` / `trait B: A`
// pair just produces `[A, B]` rather than looping.
pub fn supertrait_closure(start: &Vec<String>, traits: &TraitTable) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    out.push(start.clone());
    let mut i = 0;
    while i < out.len() {
        if let Some(entry) = trait_lookup(traits, &out[i]) {
            let mut s = 0;
            while s < entry.supertraits.len() {
                let sup = &entry.supertraits[s];
                let mut already = false;
                let mut j = 0;
                while j < out.len() {
                    if &out[j] == sup {
                        already = true;
                        break;
                    }
                    j += 1;
                }
                if !already {
                    out.push(sup.clone());
                }
                s += 1;
            }
        }
        i += 1;
    }
    out
}
