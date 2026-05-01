use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, Path,
    PathSegment, Pattern, Stmt, StructLit, Type, TypeKind,
};
use crate::span::{Error, Span};

// ----- Public RType used by borrowck and codegen -----

pub enum IntKind {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    U128,
    I128,
    Usize,
    Isize,
}

pub fn int_kind_copy(k: &IntKind) -> IntKind {
    match k {
        IntKind::U8 => IntKind::U8,
        IntKind::I8 => IntKind::I8,
        IntKind::U16 => IntKind::U16,
        IntKind::I16 => IntKind::I16,
        IntKind::U32 => IntKind::U32,
        IntKind::I32 => IntKind::I32,
        IntKind::U64 => IntKind::U64,
        IntKind::I64 => IntKind::I64,
        IntKind::U128 => IntKind::U128,
        IntKind::I128 => IntKind::I128,
        IntKind::Usize => IntKind::Usize,
        IntKind::Isize => IntKind::Isize,
    }
}

pub fn int_kind_eq(a: &IntKind, b: &IntKind) -> bool {
    match (a, b) {
        (IntKind::U8, IntKind::U8) => true,
        (IntKind::I8, IntKind::I8) => true,
        (IntKind::U16, IntKind::U16) => true,
        (IntKind::I16, IntKind::I16) => true,
        (IntKind::U32, IntKind::U32) => true,
        (IntKind::I32, IntKind::I32) => true,
        (IntKind::U64, IntKind::U64) => true,
        (IntKind::I64, IntKind::I64) => true,
        (IntKind::U128, IntKind::U128) => true,
        (IntKind::I128, IntKind::I128) => true,
        (IntKind::Usize, IntKind::Usize) => true,
        (IntKind::Isize, IntKind::Isize) => true,
        _ => false,
    }
}

pub fn int_kind_name(k: &IntKind) -> &'static str {
    match k {
        IntKind::U8 => "u8",
        IntKind::I8 => "i8",
        IntKind::U16 => "u16",
        IntKind::I16 => "i16",
        IntKind::U32 => "u32",
        IntKind::I32 => "i32",
        IntKind::U64 => "u64",
        IntKind::I64 => "i64",
        IntKind::U128 => "u128",
        IntKind::I128 => "i128",
        IntKind::Usize => "usize",
        IntKind::Isize => "isize",
    }
}

fn int_kind_from_name(name: &str) -> Option<IntKind> {
    match name {
        "u8" => Some(IntKind::U8),
        "i8" => Some(IntKind::I8),
        "u16" => Some(IntKind::U16),
        "i16" => Some(IntKind::I16),
        "u32" => Some(IntKind::U32),
        "i32" => Some(IntKind::I32),
        "u64" => Some(IntKind::U64),
        "i64" => Some(IntKind::I64),
        "u128" => Some(IntKind::U128),
        "i128" => Some(IntKind::I128),
        "usize" => Some(IntKind::Usize),
        "isize" => Some(IntKind::Isize),
        _ => None,
    }
}

// Maximum value that fits in this integer kind. We don't have negative literals,
// so we only care about the non-negative range.
fn int_kind_max(k: &IntKind) -> u128 {
    match k {
        IntKind::U8 => u8::MAX as u128,
        IntKind::I8 => i8::MAX as u128,
        IntKind::U16 => u16::MAX as u128,
        IntKind::I16 => i16::MAX as u128,
        IntKind::U32 => u32::MAX as u128,
        IntKind::I32 => i32::MAX as u128,
        IntKind::U64 => u64::MAX as u128,
        IntKind::I64 => i64::MAX as u128,
        IntKind::U128 => u128::MAX,
        IntKind::I128 => i128::MAX as u128,
        // wasm32: usize/isize are 32-bit.
        IntKind::Usize => u32::MAX as u128,
        IntKind::Isize => i32::MAX as u128,
    }
}

pub enum RType {
    Int(IntKind),
    Struct {
        path: Vec<String>,
        type_args: Vec<RType>,
        // Lifetimes provided to the struct's `<'a, ...>` params, in order.
        // Empty for non-lifetime-generic structs. Carry-only for now —
        // borrowck reads them via `find_lifetime_source` to propagate
        // borrows when a returned ref's lifetime ties to one of these.
        lifetime_args: Vec<LifetimeRepr>,
    },
    Ref {
        inner: Box<RType>,
        mutable: bool,
        // Phase B: structural carry only. `Named(_)` records a user-written
        // `'a` annotation; `Inferred(_)` is a placeholder for elided refs.
        // Type equality and unification currently ignore this field — Phase C
        // is when lifetimes start participating in any check.
        lifetime: LifetimeRepr,
    },
    RawPtr { inner: Box<RType>, mutable: bool },
    // An opaque type parameter inside a generic body. Carries the param's
    // name. Codegen substitutes these to concrete types during monomorphization;
    // operations needing layout (byte_size_of, flatten_rtype) reject `Param`.
    Param(String),
    Bool,
    // Tuple type. Empty Vec is the unit type `()` — the type of
    // value-less expressions (function bodies without a tail, blocks
    // ending in `;`, ifs without an else).
    Tuple(Vec<RType>),
    // Enum type. Layout is tagged-union: i32 discriminant at offset 0
    // followed by max-payload-byte buffer. Enum *values* live at a
    // memory address (always shadow-stack-resident); the wasm-flat
    // representation is a single `i32` (the address). Function returns
    // of enum type use the sret convention — a leading i32 out-pointer
    // param, no wasm result. See codegen for layout helpers.
    Enum {
        path: Vec<String>,
        type_args: Vec<RType>,
        lifetime_args: Vec<LifetimeRepr>,
    },
}

#[derive(Clone)]
pub enum LifetimeRepr {
    // A `'name` annotation written in source. Resolution is by-name only:
    // the named lifetime must be in scope at the type's appearance site
    // (Phase C will enforce that; Phase B accepts any name).
    Named(String),
    // A lifetime allocated for an elided / inferred reference. Phase B uses
    // 0 as a placeholder for everything; Phase C allocates fresh ids per
    // function so different elided refs are distinguishable.
    Inferred(u32),
}

pub fn lifetime_repr_clone(lr: &LifetimeRepr) -> LifetimeRepr {
    match lr {
        LifetimeRepr::Named(n) => LifetimeRepr::Named(n.clone()),
        LifetimeRepr::Inferred(id) => LifetimeRepr::Inferred(*id),
    }
}

pub fn lifetime_repr_vec_clone(v: &Vec<LifetimeRepr>) -> Vec<LifetimeRepr> {
    let mut out: Vec<LifetimeRepr> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(lifetime_repr_clone(&v[i]));
        i += 1;
    }
    out
}

pub fn lifetime_repr_eq(a: &LifetimeRepr, b: &LifetimeRepr) -> bool {
    match (a, b) {
        (LifetimeRepr::Named(na), LifetimeRepr::Named(nb)) => na == nb,
        (LifetimeRepr::Inferred(ia), LifetimeRepr::Inferred(ib)) => ia == ib,
        _ => false,
    }
}

// T5.5: whether `t` (an InferType, possibly partially resolved) can
// satisfy `std::Num`. Used by `Subst::bind_var` when an integer-literal
// var is being unified with `t` — admits any of:
// - `Int(_)`: stdlib provides `impl Num for u8/i8/.../isize`.
// - `Var(_)`: unconstrained; the caller propagates the literal flag.
// - `Param(name)`: name's bound list (via `type_param_bounds`) must
//   include `std::Num`.
// - `Struct{...}`: fully concrete enough for `solve_impl(Num, _, …)`
//   to find an impl — might not succeed if inner Vars are unresolved.
// Refs and raw pointers don't satisfy.
fn satisfies_num(
    t: &InferType,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bounds: &Vec<Vec<Vec<String>>>,
) -> bool {
    let num_path = vec!["std".to_string(), "ops".to_string(), "Num".to_string()];
    match t {
        InferType::Int(_) | InferType::Var(_) => true,
        InferType::Param(_) | InferType::Struct { .. } => {
            let rt = infer_to_rtype_for_check(t);
            solve_impl_in_ctx(&num_path, &rt, traits, type_params, type_param_bounds, 0).is_some()
        }
        InferType::Ref { .. }
        | InferType::RawPtr { .. }
        | InferType::Bool
        | InferType::Tuple(_)
        | InferType::Enum { .. } => false,
    }
}

// Convert an `InferType` to an `RType` for the purposes of impl-table
// lookup. Unresolved Vars become `RType::Int(I32)` (the literal
// default) so that `solve_impl` has something to match against; this is
// a best-effort heuristic for the bound-check path only and isn't used
// for actual type assignment.
fn infer_to_rtype_for_check(t: &InferType) -> RType {
    match t {
        InferType::Var(_) => RType::Int(IntKind::I32),
        InferType::Int(k) => RType::Int(int_kind_copy(k)),
        InferType::Struct { path, type_args, lifetime_args } => {
            let mut args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                args.push(infer_to_rtype_for_check(&type_args[i]));
                i += 1;
            }
            RType::Struct {
                path: clone_path(path),
                type_args: args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
        InferType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(infer_to_rtype_for_check(inner)),
            mutable: *mutable,
            lifetime: lifetime_repr_clone(lifetime),
        },
        InferType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(infer_to_rtype_for_check(inner)),
            mutable: *mutable,
        },
        InferType::Param(n) => RType::Param(n.clone()),
        InferType::Bool => RType::Bool,
        InferType::Tuple(elems) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(infer_to_rtype_for_check(&elems[i]));
                i += 1;
            }
            RType::Tuple(out)
        }
        InferType::Enum { path, type_args, lifetime_args } => {
            let mut args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                args.push(infer_to_rtype_for_check(&type_args[i]));
                i += 1;
            }
            RType::Enum {
                path: clone_path(path),
                type_args: args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
    }
}

// Outermost lifetime of a ref type. Returns None for non-ref types.
pub fn outer_lifetime(rt: &RType) -> Option<LifetimeRepr> {
    match rt {
        RType::Ref { lifetime, .. } => Some(lifetime_repr_clone(lifetime)),
        _ => None,
    }
}

pub fn clone_param_lifetimes(
    pls: &Vec<Option<LifetimeRepr>>,
) -> Vec<Option<LifetimeRepr>> {
    let mut out: Vec<Option<LifetimeRepr>> = Vec::new();
    let mut i = 0;
    while i < pls.len() {
        out.push(pls[i].as_ref().map(lifetime_repr_clone));
        i += 1;
    }
    out
}

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
                        if path_eq(&closure[k], trait_path) {
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
        if !path_eq(&row.trait_path, trait_path) {
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
                    bound_concrete = Some(rtype_clone(&subst[k].1));
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
    match (pattern, concrete) {
        (RType::Param(name), c) => {
            // Already bound? Must equal.
            let mut i = 0;
            while i < subst.len() {
                if subst[i].0 == *name {
                    return rtype_eq(&subst[i].1, c);
                }
                i += 1;
            }
            subst.push((name.clone(), rtype_clone(c)));
            true
        }
        (RType::Int(ka), RType::Int(kb)) => int_kind_eq(ka, kb),
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
            if !path_eq(pa, pb) || aa.len() != ab.len() {
                return false;
            }
            let mut i = 0;
            while i < aa.len() {
                if !try_match_rtype(&aa[i], &ab[i], subst) {
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
        ) => ma == mb && try_match_rtype(ia, ib, subst),
        (
            RType::RawPtr {
                inner: ia,
                mutable: ma,
            },
            RType::RawPtr {
                inner: ib,
                mutable: mb,
            },
        ) => ma == mb && try_match_rtype(ia, ib, subst),
        (RType::Bool, RType::Bool) => true,
        (RType::Tuple(a), RType::Tuple(b)) => {
            if a.len() != b.len() {
                return false;
            }
            let mut i = 0;
            while i < a.len() {
                if !try_match_rtype(&a[i], &b[i], subst) {
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
            if !path_eq(pa, pb) || aa.len() != ab.len() {
                return false;
            }
            let mut i = 0;
            while i < aa.len() {
                if !try_match_rtype(&aa[i], &ab[i], subst) {
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
fn try_match_against_infer(
    pattern: &RType,
    concrete: &InferType,
    subst: &Subst,
    env: &mut Vec<(String, InferType)>,
    pending: &mut Vec<(InferType, InferType)>,
) -> bool {
    let resolved = subst.substitute(concrete);
    match pattern {
        RType::Param(name) => {
            // Already bound? Stage a unification with the prior binding.
            let mut existing: Option<InferType> = None;
            let mut k = 0;
            while k < env.len() {
                if env[k].0 == *name {
                    existing = Some(infer_clone(&env[k].1));
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
            InferType::Int(kb) => int_kind_eq(ka, kb),
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
                if !path_eq(pa, pb) || aa.len() != ab.len() {
                    return false;
                }
                let mut i = 0;
                while i < aa.len() {
                    if !try_match_against_infer(&aa[i], &ab[i], subst, env, pending) {
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
            } => ma == mb && try_match_against_infer(ia, ib, subst, env, pending),
            _ => false,
        },
        RType::RawPtr {
            inner: ia,
            mutable: ma,
        } => match &resolved {
            InferType::RawPtr {
                inner: ib,
                mutable: mb,
            } => ma == mb && try_match_against_infer(ia, ib, subst, env, pending),
            _ => false,
        },
        RType::Tuple(pa) => match &resolved {
            InferType::Tuple(pb) => {
                if pa.len() != pb.len() {
                    return false;
                }
                let mut i = 0;
                while i < pa.len() {
                    if !try_match_against_infer(&pa[i], &pb[i], subst, env, pending) {
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
                if !path_eq(pa, pb) || aa.len() != ab.len() {
                    return false;
                }
                let mut i = 0;
                while i < aa.len() {
                    if !try_match_against_infer(&aa[i], &ab[i], subst, env, pending) {
                        return false;
                    }
                    i += 1;
                }
                true
            }
            _ => false,
        },
    }
}

// Returns indices of every param whose outermost ref lifetime equals
// `target`. Empty if no param matches. Phase D returns multiple matches:
// when `'a` ties multiple ref params to the return, all those args'
// borrows propagate into the result (the "combined borrow sets" rule).
pub fn find_lifetime_source(
    param_lifetimes: &Vec<Option<LifetimeRepr>>,
    target: &LifetimeRepr,
) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < param_lifetimes.len() {
        if let Some(plt) = &param_lifetimes[i] {
            if lifetime_repr_eq(plt, target) {
                out.push(i);
            }
        }
        i += 1;
    }
    out
}

// Walks an RType, replacing every `LifetimeRepr::Inferred(0)` placeholder
// with a fresh `Inferred(N)` allocated from `next_id`. Used per function
// during signature resolution to give each elided ref a unique id. Also
// freshens struct `lifetime_args` so e.g. `Wrapper<'_, T>` elided slots
// get distinct ids.
fn freshen_inferred_lifetimes(rt: &mut RType, next_id: &mut u32) {
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            if let LifetimeRepr::Inferred(id) = lifetime {
                if *id == 0 {
                    *id = *next_id;
                    *next_id += 1;
                }
            }
            freshen_inferred_lifetimes(inner, next_id);
        }
        RType::RawPtr { inner, .. } => freshen_inferred_lifetimes(inner, next_id),
        RType::Struct { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if let LifetimeRepr::Inferred(id) = &mut lifetime_args[i] {
                    if *id == 0 {
                        *id = *next_id;
                        *next_id += 1;
                    }
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                freshen_inferred_lifetimes(&mut type_args[i], next_id);
                i += 1;
            }
        }
        RType::Int(_) | RType::Param(_) | RType::Bool => {}
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                freshen_inferred_lifetimes(&mut elems[i], next_id);
                i += 1;
            }
        }
        RType::Enum { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if let LifetimeRepr::Inferred(id) = &mut lifetime_args[i] {
                    if *id == 0 {
                        *id = *next_id;
                        *next_id += 1;
                    }
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                freshen_inferred_lifetimes(&mut type_args[i], next_id);
                i += 1;
            }
        }
    }
}

// Rejects an `RType` carrying any `LifetimeRepr::Inferred(_)` lifetime.
// Used for struct field types — Rust requires explicit lifetime annotations
// on refs inside struct fields, so an elided lifetime there is an error.
fn require_no_inferred_lifetimes(
    rt: &RType,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            if matches!(lifetime, LifetimeRepr::Inferred(_)) {
                return Err(Error {
                    file: file.to_string(),
                    message: "missing lifetime specifier on reference in struct field"
                        .to_string(),
                    span: span.copy(),
                });
            }
            require_no_inferred_lifetimes(inner, span, file)
        }
        RType::RawPtr { inner, .. } => require_no_inferred_lifetimes(inner, span, file),
        RType::Struct { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if matches!(lifetime_args[i], LifetimeRepr::Inferred(_)) {
                    return Err(Error {
                        file: file.to_string(),
                        message: "missing lifetime argument on struct in field type"
                            .to_string(),
                        span: span.copy(),
                    });
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                require_no_inferred_lifetimes(&type_args[i], span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Int(_) | RType::Param(_) | RType::Bool => Ok(()),
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                require_no_inferred_lifetimes(&elems[i], span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Enum { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if matches!(lifetime_args[i], LifetimeRepr::Inferred(_)) {
                    return Err(Error {
                        file: file.to_string(),
                        message: "missing lifetime argument on enum in field type"
                            .to_string(),
                        span: span.copy(),
                    });
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                require_no_inferred_lifetimes(&type_args[i], span, file)?;
                i += 1;
            }
            Ok(())
        }
    }
}

// Validates that every `LifetimeRepr::Named` inside an `RType` references a
// lifetime declared in `lifetime_params`. Used to reject signatures that
// reference an undeclared `'a`.
fn validate_named_lifetimes(
    rt: &RType,
    lifetime_params: &Vec<String>,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            check_named_in_scope(lifetime, lifetime_params, span, file)?;
            validate_named_lifetimes(inner, lifetime_params, span, file)
        }
        RType::RawPtr { inner, .. } => {
            validate_named_lifetimes(inner, lifetime_params, span, file)
        }
        RType::Struct { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                check_named_in_scope(&lifetime_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                validate_named_lifetimes(&type_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Int(_) | RType::Param(_) | RType::Bool => Ok(()),
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                validate_named_lifetimes(&elems[i], lifetime_params, span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Enum { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                check_named_in_scope(&lifetime_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                validate_named_lifetimes(&type_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            Ok(())
        }
    }
}

fn check_named_in_scope(
    lt: &LifetimeRepr,
    lifetime_params: &Vec<String>,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    if let LifetimeRepr::Named(name) = lt {
        let mut found = false;
        let mut i = 0;
        while i < lifetime_params.len() {
            if lifetime_params[i] == *name {
                found = true;
                break;
            }
            i += 1;
        }
        if !found {
            return Err(Error {
                file: file.to_string(),
                message: format!("undeclared lifetime `'{}`", name),
                span: span.copy(),
            });
        }
    }
    Ok(())
}

pub fn rtype_clone(t: &RType) -> RType {
    match t {
        RType::Int(k) => RType::Int(int_kind_copy(k)),
        RType::Struct { path, type_args, lifetime_args } => RType::Struct {
            path: clone_path(path),
            type_args: rtype_vec_clone(type_args),
            lifetime_args: lifetime_repr_vec_clone(lifetime_args),
        },
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(rtype_clone(inner)),
            mutable: *mutable,
            lifetime: lifetime_repr_clone(lifetime),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(rtype_clone(inner)),
            mutable: *mutable,
        },
        RType::Param(n) => RType::Param(n.clone()),
        RType::Bool => RType::Bool,
        RType::Tuple(elems) => RType::Tuple(rtype_vec_clone(elems)),
        RType::Enum { path, type_args, lifetime_args } => RType::Enum {
            path: clone_path(path),
            type_args: rtype_vec_clone(type_args),
            lifetime_args: lifetime_repr_vec_clone(lifetime_args),
        },
    }
}

fn rtype_vec_clone(v: &Vec<RType>) -> Vec<RType> {
    let mut out: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(rtype_clone(&v[i]));
        i += 1;
    }
    out
}

fn rtype_vec_eq(a: &Vec<RType>, b: &Vec<RType>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !rtype_eq(&a[i], &b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

pub fn rtype_eq(a: &RType, b: &RType) -> bool {
    match (a, b) {
        (RType::Bool, RType::Bool) => true,
        (RType::Int(ka), RType::Int(kb)) => int_kind_eq(ka, kb),
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
        ) => path_eq(pa, pb) && rtype_vec_eq(aa, ab),
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
        ) => ma == mb && rtype_eq(ia, ib),
        (
            RType::RawPtr {
                inner: ia,
                mutable: ma,
            },
            RType::RawPtr {
                inner: ib,
                mutable: mb,
            },
        ) => ma == mb && rtype_eq(ia, ib),
        (RType::Param(a), RType::Param(b)) => a == b,
        (RType::Tuple(a), RType::Tuple(b)) => rtype_vec_eq(a, b),
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
        ) => path_eq(pa, pb) && rtype_vec_eq(aa, ab),
        _ => false,
    }
}

pub fn rtype_to_string(t: &RType) -> String {
    match t {
        RType::Bool => "bool".to_string(),
        RType::Int(k) => int_kind_name(k).to_string(),
        RType::Struct { path, type_args, .. } => {
            if type_args.is_empty() {
                place_to_string(path)
            } else {
                let mut s = place_to_string(path);
                s.push('<');
                let mut i = 0;
                while i < type_args.len() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&rtype_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
        RType::Ref { inner, mutable, .. } => {
            if *mutable {
                format!("&mut {}", rtype_to_string(inner))
            } else {
                format!("&{}", rtype_to_string(inner))
            }
        }
        RType::RawPtr { inner, mutable } => {
            if *mutable {
                format!("*mut {}", rtype_to_string(inner))
            } else {
                format!("*const {}", rtype_to_string(inner))
            }
        }
        RType::Param(n) => n.clone(),
        RType::Tuple(elems) => {
            let mut s = String::from("(");
            let mut i = 0;
            while i < elems.len() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&rtype_to_string(&elems[i]));
                i += 1;
            }
            // Trailing comma for 1-tuples (matches Rust output).
            if elems.len() == 1 {
                s.push(',');
            }
            s.push(')');
            s
        }
        RType::Enum { path, type_args, .. } => {
            if type_args.is_empty() {
                place_to_string(path)
            } else {
                let mut s = place_to_string(path);
                s.push('<');
                let mut i = 0;
                while i < type_args.len() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&rtype_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
    }
}

pub fn rtype_size(ty: &RType, structs: &StructTable) -> u32 {
    match ty {
        RType::Bool => 1,
        RType::Int(k) => match k {
            IntKind::U128 | IntKind::I128 => 2,
            _ => 1,
        },
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut s: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                s += rtype_size(&fty, structs);
                i += 1;
            }
            s
        }
        RType::Ref { .. } | RType::RawPtr { .. } => 1,
        RType::Param(_) => unreachable!("rtype_size called on unresolved type parameter"),
        RType::Tuple(elems) => {
            let mut s: u32 = 0;
            let mut i = 0;
            while i < elems.len() {
                s += rtype_size(&elems[i], structs);
                i += 1;
            }
            s
        }
        // Enum values are represented on the wasm stack as a single
        // i32 address. The actual bytes (disc + payload) live at that
        // address. See `byte_size_of` for the on-stack-frame size and
        // `flatten_rtype` for the matching wasm shape.
        RType::Enum { .. } => 1,
    }
}

fn struct_env(type_params: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), rtype_clone(&type_args[i])));
        i += 1;
    }
    env
}

pub fn flatten_rtype(ty: &RType, structs: &StructTable, out: &mut Vec<crate::wasm::ValType>) {
    match ty {
        RType::Bool => out.push(crate::wasm::ValType::I32),
        RType::Int(k) => match k {
            IntKind::U64 | IntKind::I64 => out.push(crate::wasm::ValType::I64),
            IntKind::U128 | IntKind::I128 => {
                out.push(crate::wasm::ValType::I64);
                out.push(crate::wasm::ValType::I64);
            }
            _ => out.push(crate::wasm::ValType::I32),
        },
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                flatten_rtype(&fty, structs, out);
                i += 1;
            }
        }
        RType::Ref { .. } | RType::RawPtr { .. } => out.push(crate::wasm::ValType::I32),
        RType::Param(_) => unreachable!("flatten_rtype called on unresolved type parameter"),
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                flatten_rtype(&elems[i], structs, out);
                i += 1;
            }
        }
        // Enums flatten to a single i32 (the address of the on-shadow-
        // stack disc+payload bytes). Construction allocates the slot
        // and yields its address; reads chase the address.
        RType::Enum { .. } => out.push(crate::wasm::ValType::I32),
    }
}

pub fn byte_size_of(rt: &RType, structs: &StructTable, enums: &EnumTable) -> u32 {
    match rt {
        RType::Bool => 1,
        RType::Int(k) => match k {
            IntKind::U8 | IntKind::I8 => 1,
            IntKind::U16 | IntKind::I16 => 2,
            IntKind::U32 | IntKind::I32 | IntKind::Usize | IntKind::Isize => 4,
            IntKind::U64 | IntKind::I64 => 8,
            IntKind::U128 | IntKind::I128 => 16,
        },
        RType::Ref { .. } | RType::RawPtr { .. } => 4,
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut total: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                total += byte_size_of(&fty, structs, enums);
                i += 1;
            }
            total
        }
        RType::Param(_) => unreachable!("byte_size_of called on unresolved type parameter"),
        RType::Tuple(elems) => {
            let mut total: u32 = 0;
            let mut i = 0;
            while i < elems.len() {
                total += byte_size_of(&elems[i], structs, enums);
                i += 1;
            }
            total
        }
        // Tagged-union: 4-byte i32 disc at offset 0, then a buffer of
        // max(payload byte size) for the largest variant. Each variant's
        // payload sits within the buffer in declaration order; smaller
        // variants leave the trailing bytes unused.
        RType::Enum { path, type_args, .. } => {
            let entry = enum_lookup(enums, path).expect("resolved enum");
            let env = struct_env(&entry.type_params, type_args);
            let mut max_payload: u32 = 0;
            let mut i = 0;
            while i < entry.variants.len() {
                let p = variant_payload_byte_size(&entry.variants[i].payload, &env, structs, enums);
                if p > max_payload {
                    max_payload = p;
                }
                i += 1;
            }
            4 + max_payload
        }
    }
}

// Sum-of-bytes for a variant's payload after substituting the enum's
// type-arg env into each field's declared type.
pub fn variant_payload_byte_size(
    payload: &VariantPayloadResolved,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
) -> u32 {
    match payload {
        VariantPayloadResolved::Unit => 0,
        VariantPayloadResolved::Tuple(types) => {
            let mut total: u32 = 0;
            let mut i = 0;
            while i < types.len() {
                let ty = substitute_rtype(&types[i], env);
                total += byte_size_of(&ty, structs, enums);
                i += 1;
            }
            total
        }
        VariantPayloadResolved::Struct(fields) => {
            let mut total: u32 = 0;
            let mut i = 0;
            while i < fields.len() {
                let ty = substitute_rtype(&fields[i].ty, env);
                total += byte_size_of(&ty, structs, enums);
                i += 1;
            }
            total
        }
    }
}

// Substitutes type parameters with their concrete types. `env` maps each
// param name to a concrete RType. Called by codegen during monomorphization.
// If a Param doesn't appear in env, returns it unchanged (for nested-generic
// scenarios where the env is partial).
pub fn substitute_rtype(rt: &RType, env: &Vec<(String, RType)>) -> RType {
    match rt {
        RType::Bool => RType::Bool,
        RType::Int(k) => RType::Int(int_kind_copy(k)),
        RType::Struct { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(substitute_rtype(&type_args[i], env));
                i += 1;
            }
            RType::Struct {
                path: clone_path(path),
                type_args: subst_args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(substitute_rtype(inner, env)),
            mutable: *mutable,
            lifetime: lifetime_repr_clone(lifetime),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(substitute_rtype(inner, env)),
            mutable: *mutable,
        },
        RType::Param(name) => {
            let mut i = 0;
            while i < env.len() {
                if env[i].0 == *name {
                    return rtype_clone(&env[i].1);
                }
                i += 1;
            }
            RType::Param(name.clone())
        }
        RType::Tuple(elems) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(substitute_rtype(&elems[i], env));
                i += 1;
            }
            RType::Tuple(out)
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(substitute_rtype(&type_args[i], env));
                i += 1;
            }
            RType::Enum {
                path: clone_path(path),
                type_args: subst_args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
    }
}

// Whether `t` implements `std::Copy`. Built-in types (integers, `&T`,
// `*const T`, `*mut T`) get their `impl Copy` rows from `lib/std/lib.rs`;
// user-written `impl Copy for Foo {}` adds rows too. `&mut T` is
// deliberately *not* Copy.
pub fn is_copy(t: &RType, traits: &TraitTable) -> bool {
    is_copy_with_bounds(t, traits, &Vec::new(), &Vec::new())
}

// Same as `is_copy` but also recognizes a `Param(name)` as Copy when the
// type-param's bounds include `std::Copy`. `type_params` and
// `type_param_bounds` align in length and order — typically threaded
// through from a `GenericTemplate.type_params` / `type_param_bounds`.
pub fn is_copy_with_bounds(
    t: &RType,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bounds: &Vec<Vec<Vec<String>>>,
) -> bool {
    solve_impl_in_ctx(&copy_trait_path(), t, traits, type_params, type_param_bounds, 0).is_some()
}

pub fn copy_trait_path() -> Vec<String> {
    vec!["std".to_string(), "marker".to_string(), "Copy".to_string()]
}

pub fn drop_trait_path() -> Vec<String> {
    vec!["std".to_string(), "ops".to_string(), "Drop".to_string()]
}

// Whether `t` implements `std::Drop`. Used by codegen to decide whether
// to emit drop calls at scope end and by impl validation to enforce
// Drop/Copy mutual exclusion.
pub fn is_drop(t: &RType, traits: &TraitTable) -> bool {
    let drop_path = drop_trait_path();
    solve_impl(&drop_path, t, traits, 0).is_some()
}

pub fn is_raw_ptr(t: &RType) -> bool {
    matches!(t, RType::RawPtr { .. })
}

pub fn is_ref_mutable(t: &RType) -> bool {
    matches!(t, RType::Ref { mutable: true, .. })
}

pub struct RTypedField {
    pub name: String,
    pub name_span: Span,
    pub ty: RType,
    pub is_pub: bool,
}

pub struct StructEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub type_params: Vec<String>,
    // Lifetime params declared on the struct (e.g., `struct Holder<'a, T>`
    // gives `lifetime_params = ["a"]`). Empty for non-lifetime-generic
    // structs. Used to validate lifetime args at type-position uses and to
    // build a substitution env when reading field types.
    pub lifetime_params: Vec<String>,
    pub fields: Vec<RTypedField>,
    pub is_pub: bool,
}

pub struct StructTable {
    pub entries: Vec<StructEntry>,
}

pub fn struct_lookup<'a>(table: &'a StructTable, path: &Vec<String>) -> Option<&'a StructEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

// Enum table — analogous to StructTable. Each entry records the enum's
// variants with their resolved payload types. Generic enums carry their
// type/lifetime param names; layout (`byte_size_of` etc.) substitutes
// type_args at use-site to compute concrete sizes.
pub struct EnumEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub type_params: Vec<String>,
    pub lifetime_params: Vec<String>,
    pub variants: Vec<EnumVariantEntry>,
    pub is_pub: bool,
}

pub struct EnumVariantEntry {
    pub name: String,
    pub name_span: Span,
    // 0-based discriminant in declaration order. Stored as u32 (we
    // emit it as i32.const at codegen).
    pub disc: u32,
    pub payload: VariantPayloadResolved,
}

pub enum VariantPayloadResolved {
    Unit,
    Tuple(Vec<RType>),
    Struct(Vec<RTypedField>),
}

pub struct EnumTable {
    pub entries: Vec<EnumEntry>,
}

pub fn enum_lookup<'a>(table: &'a EnumTable, path: &Vec<String>) -> Option<&'a EnumEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

// Re-export entry: a `pub use foo::Bar;` in module M makes the name
// `M::Bar` (or `M::<rename>` for `pub use foo::Bar as Q;`) resolve
// to `foo::Bar`. The table lets cross-module path lookups follow
// these re-exports — without it, outside callers would have to know
// the original definition's path even when the re-export is the
// public API.
#[derive(Clone)]
pub struct ReExport {
    pub module: Vec<String>,
    pub local_name: String,
    pub target: Vec<String>,
}

pub struct ReExportTable {
    pub entries: Vec<ReExport>,
}

// Walk every module recursively, collecting every `pub use ...`
// entry. Each `pub use foo::Bar;` (or renamed) in module M produces a
// ReExport entry. Globs `pub use foo::*;` register a wildcard re-
// export that's expanded lazily at lookup time.
pub fn build_reexport_table(root: &crate::ast::Module) -> ReExportTable {
    let mut table = ReExportTable { entries: Vec::new() };
    let mut path: Vec<String> = Vec::new();
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
    let crate_root: String = if path.is_empty() {
        String::new()
    } else {
        path[0].clone()
    };
    collect_reexports_in_module(root, &mut path, &crate_root, &mut table);
    table
}

fn collect_reexports_in_module(
    module: &crate::ast::Module,
    path: &mut Vec<String>,
    crate_root: &str,
    table: &mut ReExportTable,
) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            crate::ast::Item::Use(u) if u.is_pub => {
                // Flatten this pub use's tree into UseEntries (with the
                // crate-root rewrite), then turn each Explicit entry
                // into a ReExport at the current module.
                let mut entries: Vec<UseEntry> = Vec::new();
                flatten_use_tree(&Vec::new(), &u.tree, crate_root, true, &mut entries);
                let mut k = 0;
                while k < entries.len() {
                    if let UseEntry::Explicit { local_name, full_path, .. } = &entries[k] {
                        table.entries.push(ReExport {
                            module: clone_path(path),
                            local_name: local_name.clone(),
                            target: clone_path(full_path),
                        });
                    }
                    // Globs: a `pub use foo::*;` would need lazy
                    // expansion at lookup time — skip for now (not in
                    // the bootstrap path). Documented as a limitation.
                    k += 1;
                }
            }
            crate::ast::Item::Module(m) => {
                path.push(m.name.clone());
                collect_reexports_in_module(m, path, crate_root, table);
                path.pop();
            }
            _ => {}
        }
        i += 1;
    }
}

// Apply re-exports to a path lookup. If `path` is `[mod..., name]`
// and `[mod...]` has a `pub use ... as name;`, return the target. May
// chain through multiple levels (a re-export of a re-export). Caller
// passes `probe` to validate the final destination resolves in their
// table; we stop chaining once probe accepts.
pub fn resolve_via_reexports<F>(
    path: &Vec<String>,
    table: &ReExportTable,
    probe: F,
) -> Option<Vec<String>>
where
    F: Fn(&Vec<String>) -> bool,
{
    if path.is_empty() {
        return None;
    }
    let mut current = clone_path(path);
    let mut depth = 0;
    while depth < 16 {
        if probe(&current) {
            return Some(current);
        }
        let module_len = current.len() - 1;
        let mut found: Option<Vec<String>> = None;
        let mut i = 0;
        while i < table.entries.len() {
            let e = &table.entries[i];
            if e.module.len() == module_len
                && e.local_name == current[module_len]
            {
                let mut module_eq = true;
                let mut k = 0;
                while k < module_len {
                    if e.module[k] != current[k] {
                        module_eq = false;
                        break;
                    }
                    k += 1;
                }
                if module_eq {
                    found = Some(clone_path(&e.target));
                    break;
                }
            }
            i += 1;
        }
        match found {
            Some(t) => {
                current = t;
                depth += 1;
            }
            None => return None,
        }
    }
    None
}

// Re-export-aware lookups. When the user writes a path that matches
// a `pub use` re-export, the actual table holds the entry under the
// canonical (re-export target) path — these helpers transparently
// follow the re-export chain so callers don't have to.
pub fn trait_lookup_resolved<'a>(
    traits: &'a TraitTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<&'a TraitEntry> {
    if let Some(e) = trait_lookup(traits, path) {
        return Some(e);
    }
    let target = resolve_via_reexports(path, reexports, |p| {
        trait_lookup(traits, p).is_some()
    })?;
    trait_lookup(traits, &target)
}

pub fn struct_lookup_resolved<'a>(
    structs: &'a StructTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<&'a StructEntry> {
    if let Some(e) = struct_lookup(structs, path) {
        return Some(e);
    }
    let target = resolve_via_reexports(path, reexports, |p| {
        struct_lookup(structs, p).is_some()
    })?;
    struct_lookup(structs, &target)
}

pub fn func_path_resolved(
    funcs: &FuncTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<Vec<String>> {
    if funcs_entry_index(funcs, path).is_some() || template_lookup(funcs, path).is_some() {
        return Some(clone_path(path));
    }
    resolve_via_reexports(path, reexports, |p| {
        funcs_entry_index(funcs, p).is_some() || template_lookup(funcs, p).is_some()
    })
}

// Visibility check: an item with `is_pub` flag, defined inside
// `defining_module`, is visible from `accessor_module` iff `is_pub`
// or `accessor_module` is `defining_module` or a descendant. Mirrors
// Rust's "private items are visible to the defining module and its
// descendants."
//
// Callers pass `defining_module` explicitly so the rule applies
// uniformly to free functions, structs, traits, and methods —
// methods nest under their impl target's name in the path, but the
// defining module is still the enclosing module, not the struct.
pub fn is_visible_from(
    defining_module: &Vec<String>,
    is_pub: bool,
    accessor_module: &Vec<String>,
) -> bool {
    if is_pub {
        return true;
    }
    if accessor_module.len() < defining_module.len() {
        return false;
    }
    let mut i = 0;
    while i < defining_module.len() {
        if accessor_module[i] != defining_module[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Defining module for a function-table path: free functions live at
// `[mod..., name]` (drop one), inherent/trait-impl methods live at
// `[mod..., StructName, method_name]` (drop two). The
// `is_method_path` flag is computed from `FnSymbol.impl_target`.
pub fn fn_defining_module(item_path: &Vec<String>, is_method: bool) -> Vec<String> {
    let drop = if is_method { 2 } else { 1 };
    let n = if item_path.len() >= drop {
        item_path.len() - drop
    } else {
        0
    };
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < n {
        out.push(item_path[i].clone());
        i += 1;
    }
    out
}

// Defining module for a struct/trait at `[mod..., name]`.
pub fn type_defining_module(item_path: &Vec<String>) -> Vec<String> {
    if item_path.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < item_path.len() {
        out.push(item_path[i].clone());
        i += 1;
    }
    out
}

// Field-level visibility: a non-pub struct field is only accessible
// from inside the struct's defining module (or any descendant).
pub fn field_visible_from(
    struct_path: &Vec<String>,
    field_is_pub: bool,
    accessor_module: &Vec<String>,
) -> bool {
    is_visible_from(
        &type_defining_module(struct_path),
        field_is_pub,
        accessor_module,
    )
}

// A flattened entry from a `use` declaration. `Explicit` corresponds
// to `use a::b::c;` (or a renamed `use a::b::c as d;`) — single name
// → single full path. `Glob` corresponds to `use a::b::*;` — every
// item directly under `a::b` is brought into scope, resolved lazily
// at lookup time via probing the relevant table.
//
// `is_pub` carries the originating `UseDecl.is_pub` — for `pub use`,
// the entry contributes to the enclosing module's re-export table
// (see `ReExportTable`) so outside modules can reach the imported
// item via `<this_module>::<local_name>`.
#[derive(Clone)]
pub enum UseEntry {
    Explicit {
        local_name: String,
        full_path: Vec<String>,
        is_pub: bool,
    },
    Glob {
        module_path: Vec<String>,
        is_pub: bool,
    },
}

// Recursively flatten a UseTree into a list of UseEntry, with `prefix`
// prepended to every contained path. Top-level callers pass an empty
// prefix; the recursion accumulates prefix segments through Nested.
//
// A leading `crate` segment in any use path is rewritten to the
// enclosing crate's root: for the user crate (root_name == "") it's
// stripped (so `use crate::foo::bar;` becomes `["foo","bar"]`); for a
// library (e.g. root_name == "std") it's substituted (so `use
// crate::Drop` inside std's own source becomes `["std","Drop"]`).
// The prefix is applied first, then the crate-rewrite acts on the
// resulting absolute path.
pub fn flatten_use_tree(
    prefix: &Vec<String>,
    tree: &crate::ast::UseTree,
    crate_root: &str,
    is_pub: bool,
    out: &mut Vec<UseEntry>,
) {
    match tree {
        crate::ast::UseTree::Leaf { path, rename, .. } => {
            let mut full = clone_path(prefix);
            let mut i = 0;
            while i < path.len() {
                full.push(path[i].clone());
                i += 1;
            }
            // Local name comes from the *original* last segment (or
            // explicit rename) — `use crate::foo::Bar;` imports `Bar`,
            // not `crate`, even after the rewrite below.
            let local_name = match rename {
                Some(r) => r.clone(),
                None => {
                    if full.is_empty() {
                        return; // nothing to import
                    }
                    full[full.len() - 1].clone()
                }
            };
            full = rewrite_crate_prefix(full, crate_root);
            out.push(UseEntry::Explicit {
                local_name,
                full_path: full,
                is_pub,
            });
        }
        crate::ast::UseTree::Glob { path, .. } => {
            let mut full = clone_path(prefix);
            let mut i = 0;
            while i < path.len() {
                full.push(path[i].clone());
                i += 1;
            }
            full = rewrite_crate_prefix(full, crate_root);
            out.push(UseEntry::Glob {
                module_path: full,
                is_pub,
            });
        }
        crate::ast::UseTree::Nested { prefix: p, children, .. } => {
            let mut combined = clone_path(prefix);
            let mut i = 0;
            while i < p.len() {
                combined.push(p[i].clone());
                i += 1;
            }
            let mut k = 0;
            while k < children.len() {
                flatten_use_tree(&combined, &children[k], crate_root, is_pub, out);
                k += 1;
            }
        }
    }
}

fn rewrite_crate_prefix(mut path: Vec<String>, crate_root: &str) -> Vec<String> {
    if !path.is_empty() && path[0] == "crate" {
        if crate_root.is_empty() {
            // User crate: drop the `crate` segment entirely. Items
            // live at the empty-prefix root, so `crate::foo::bar`
            // becomes just `foo::bar`.
            let mut rest: Vec<String> = Vec::new();
            let mut i = 1;
            while i < path.len() {
                rest.push(path[i].clone());
                i += 1;
            }
            return rest;
        } else {
            // Library: substitute `crate` → library name. So inside
            // `std`'s source, `use crate::Drop;` becomes `std::Drop`.
            path[0] = crate_root.to_string();
            return path;
        }
    }
    path
}

// Apply use-table resolution to a path. Looks at the path's first
// segment; if it matches an explicit use, the imported full path
// replaces just that first segment (the rest of the path is appended).
// If no explicit match, each glob in scope is tried by prefixing the
// glob's module path to the original path and probing the resulting
// candidate against the caller's lookup target. Returns the
// use-resolved path, or `None` if no use entry applied.
//
// `scope` is a single flat list of `UseEntry`s, ordered with
// outermost-first / innermost-last; iteration is reverse so the
// innermost scope's entries shadow outer ones.
//
// Examples (with `use std::Drop;` and `use std::*;`):
//   - `Drop` → `std::Drop` (explicit match, single segment).
//   - `Pair::new` (with `use foo::Pair;`) → `foo::Pair::new` (the
//     imported `Pair` becomes the path root; the rest follows).
//   - `Drop` (with only `use std::*;`, no explicit) → `std::Drop`
//     iff probe(["std","Drop"]) succeeds.
pub fn resolve_via_use_scopes<F>(
    path: &[String],
    scope: &Vec<UseEntry>,
    probe: F,
) -> Option<Vec<String>>
where
    F: Fn(&Vec<String>) -> bool,
{
    if path.is_empty() {
        return None;
    }
    let head = &path[0];
    // Explicit match on the first segment — innermost (last-pushed) wins.
    let mut s = scope.len();
    while s > 0 {
        s -= 1;
        if let UseEntry::Explicit { local_name, full_path, .. } = &scope[s] {
            if local_name == head {
                let mut out = clone_path(full_path);
                let mut j = 1;
                while j < path.len() {
                    out.push(path[j].clone());
                    j += 1;
                }
                return Some(out);
            }
        }
    }
    // No explicit; try each glob's `module_path :: path` in reverse.
    let mut s = scope.len();
    while s > 0 {
        s -= 1;
        if let UseEntry::Glob { module_path, .. } = &scope[s] {
            let mut candidate = clone_path(module_path);
            let mut j = 0;
            while j < path.len() {
                candidate.push(path[j].clone());
                j += 1;
            }
            if probe(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

// Walk a Module's items and flatten every `use` declaration into a
// single `Vec<UseEntry>`. `crate_root` is the enclosing crate's name
// (empty for the user crate, or e.g. `"std"` for a library), used by
// `flatten_use_tree` to rewrite leading `crate` segments. Submodule
// uses don't propagate up.
pub fn module_use_entries(module: &crate::ast::Module, crate_root: &str) -> Vec<UseEntry> {
    let mut out: Vec<UseEntry> = Vec::new();
    let mut i = 0;
    while i < module.items.len() {
        if let crate::ast::Item::Use(u) = &module.items[i] {
            flatten_use_tree(&Vec::new(), &u.tree, crate_root, u.is_pub, &mut out);
        }
        i += 1;
    }
    out
}

// Per-place move state recorded by borrowck. `Moved` means moved on
// every reachable path; `MaybeMoved` means moved on some paths but not
// others (the binding's storage is potentially-init at the place's
// scope-end, requiring a runtime drop flag in codegen). The implicit
// third state — `Init` — is "the place isn't in the list at all."
#[derive(Clone, PartialEq, Eq)]
pub enum MoveStatus {
    Moved,
    MaybeMoved,
}

#[derive(Clone)]
pub struct MovedPlace {
    pub place: Vec<String>,
    pub status: MoveStatus,
}

// Trait declarations registered during the first typeck pass. Trait
// methods' signatures are stored with `Self` as `RType::Param("Self")` so
// impl validation can substitute against the impl target.
pub struct TraitTable {
    pub entries: Vec<TraitEntry>,
    // Each `impl Trait for Target` row registered. Multiple rows for the
    // same `(trait_path, target_pattern)` are rejected as duplicates.
    pub impls: Vec<TraitImplEntry>,
}

pub struct TraitEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub methods: Vec<TraitMethodEntry>,
    pub is_pub: bool,
    pub supertraits: Vec<Vec<String>>,
}

pub struct TraitMethodEntry {
    pub name: String,
    pub name_span: Span,
    // Method-level type-params declared on the trait method (e.g. `fn
    // bar<U>(self, u: U)`). Names appear in `param_types` / `return_type`
    // as `RType::Param(name)`. Validation against impl methods compares
    // by arity + α-equivalence (impl's `<V>` matched positionally with
    // trait's `<U>`); symbolic dispatch allocates fresh inference vars
    // per call, optionally pinned by turbofish.
    pub type_params: Vec<String>,
    // Resolved param types in declaration order. Param 0 is the receiver
    // (when the method has one); `Self` appears as `RType::Param("Self")`
    // and gets substituted with the impl target during validation +
    // dispatch.
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    // Receiver shape if param 0 is a `self` receiver — Move (`self:
    // Self`), BorrowImm (`&Self`), or BorrowMut (`&mut Self`). None for
    // associated functions without a receiver.
    pub receiver_shape: Option<TraitReceiverShape>,
}

#[derive(Clone, Copy)]
pub enum TraitReceiverShape {
    Move,
    BorrowImm,
    BorrowMut,
}

// One `impl Trait for Target` row. `target` is the impl-target pattern
// (as in inherent impls — see `FnSymbol.impl_target`); `impl_type_params`
// records the impl's own type-params (not the trait's).
pub struct TraitImplEntry {
    pub trait_path: Vec<String>,
    pub target: RType,
    pub impl_type_params: Vec<String>,
    pub impl_lifetime_params: Vec<String>,
    // Per impl-type-param trait bounds (resolved). Same shape and order as
    // `impl_type_params`. `solve_impl` enforces these recursively when
    // matching a candidate impl against a concrete type.
    pub impl_type_param_bounds: Vec<Vec<Vec<String>>>,
    pub file: String,
    pub span: Span,
}

pub fn trait_lookup<'a>(table: &'a TraitTable, path: &Vec<String>) -> Option<&'a TraitEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
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
    out.push(clone_path(start));
    let mut i = 0;
    while i < out.len() {
        if let Some(entry) = trait_lookup(traits, &out[i]) {
            let mut s = 0;
            while s < entry.supertraits.len() {
                let sup = &entry.supertraits[s];
                let mut already = false;
                let mut j = 0;
                while j < out.len() {
                    if path_eq(&out[j], sup) {
                        already = true;
                        break;
                    }
                    j += 1;
                }
                if !already {
                    out.push(clone_path(sup));
                }
                s += 1;
            }
        }
        i += 1;
    }
    out
}

pub struct FnSymbol {
    pub path: Vec<String>,
    pub idx: u32,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    // For trait-impl methods, the index into `TraitTable.impls` of the
    // owning impl row. None for free fns and inherent methods.
    pub trait_impl_idx: Option<usize>,
    pub is_pub: bool,
    // Per `Expr` node, indexed by `Expr.id`. Contains the resolved `RType`
    // for nodes that carry a value type. `None` for nodes without one
    // (currently unused — every Expr produces a value in our subset).
    // Borrowck reads this for binding types (via `let_stmt.value.id`),
    // codegen reads this for layout (let bindings, lit constants, struct
    // literals), safeck reads `Deref(inner).inner.id`'s entry to detect
    // raw-pointer derefs.
    pub expr_types: Vec<Option<RType>>,
    // Outermost lifetime of each param's ref type, or None for non-ref
    // params. Used by borrowck to map a returned ref's lifetime back to the
    // arg slot(s) whose borrows it inherits.
    pub param_lifetimes: Vec<Option<LifetimeRepr>>,
    // Outermost lifetime of the return ref, or None if the return type isn't
    // a ref. Set by lifetime elision (or copied from a user `'a` annotation).
    pub ret_lifetime: Option<LifetimeRepr>,
    // For methods (registered inside an `impl Target { ... }` block): the
    // impl's target type pattern. `None` for free functions. The pattern may
    // contain `RType::Param(impl_param_name)` slots that get bound by
    // matching against the receiver type at each call site.
    pub impl_target: Option<RType>,
    // Per `MethodCall` expression, indexed by Expr.id. Some(_) at MethodCall
    // node ids; None elsewhere.
    pub method_resolutions: Vec<Option<MethodResolution>>,
    // Per `Call` expression, indexed by Expr.id.
    pub call_resolutions: Vec<Option<CallResolution>>,
    // T4.6: places whose move-state at the binding's scope-end was non-Init,
    // snapshotted from borrowck's walk. Codegen consults this to decide what
    // to do at each Drop binding's drop point: `Init` means the binding
    // wasn't moved at all (unconditional drop); `Moved` means it was moved on
    // every path (skip drop); `MaybeMoved` means it was moved on some paths
    // (emit a runtime drop flag — set 1 at decl, 0 at every move site, drop
    // gated on flag).
    pub moved_places: Vec<MovedPlace>,
    // Per whole-binding move site: every (NodeId, binding-name) pair where
    // borrowck observed a non-Copy whole-binding read that consumed the
    // binding's storage. Codegen consults this to clear drop flags: at the
    // codegen for the matching NodeId, emit `flag = 0` for the named
    // binding (only when that binding's status at scope-end is MaybeMoved
    // — Init bindings don't have flags, and Moved bindings drop is just
    // skipped). Empty for fns with no whole-binding moves.
    pub move_sites: Vec<(crate::ast::NodeId, String)>,
}

// How a `Call` expression resolves to a callee. For non-generic functions
// it's an index into FuncTable.entries. For generic functions, it points to
// a template plus the type arguments at the call site (which may themselves
// contain `Param` if the calling function is also generic — substituted at
// monomorphization).
pub enum CallResolution {
    Direct(usize),
    Generic {
        template_idx: usize,
        type_args: Vec<RType>,
    },
    // Enum variant construction: `Path::Variant(args...)` produces an
    // enum value. `enum_path` is the canonical enum's path; `disc` is
    // the variant index; `type_args` are the enum's type-args at this
    // construction site (substituted under any outer monomorphization
    // env at codegen time).
    Variant {
        enum_path: Vec<String>,
        disc: u32,
        type_args: Vec<RType>,
    },
}

// A generic function declaration. Its body is type-checked once,
// polymorphically (so let_types/lit_types/etc. may contain `RType::Param`).
// Codegen monomorphizes lazily per (template_idx, concrete type_args) pair,
// substituting Param → concrete in the recorded artifacts.
pub struct GenericTemplate {
    pub path: Vec<String>,
    pub type_params: Vec<String>,
    // Per type-param trait bounds (resolved to trait paths), in the same
    // order as `type_params`. Each inner Vec is the bound list for that
    // type-param. Used by symbolic trait-method dispatch in generic
    // bodies (`fn f<T: Show>(t: T) { t.show() }`).
    pub type_param_bounds: Vec<Vec<Vec<String>>>,
    // Number of leading entries in `type_params` that come from the
    // enclosing `impl<...>` block (the rest are the method's own type
    // params). Zero for free generic functions.
    pub impl_type_param_count: usize,
    // For trait-impl methods, the index into `TraitTable.impls`. None
    // for free fns and inherent methods.
    pub trait_impl_idx: Option<usize>,
    pub is_pub: bool,
    pub func: crate::ast::Function,
    pub enclosing_module: Vec<String>,
    pub source_file: String,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub expr_types: Vec<Option<RType>>,
    pub param_lifetimes: Vec<Option<LifetimeRepr>>,
    pub ret_lifetime: Option<LifetimeRepr>,
    // For impl methods: the impl's target type pattern (see FnSymbol).
    // `None` for free generic functions.
    pub impl_target: Option<RType>,
    pub method_resolutions: Vec<Option<MethodResolution>>,
    pub call_resolutions: Vec<Option<CallResolution>>,
    // T4.6: see FnSymbol.moved_places. For templates the snapshot is taken
    // from the polymorphic body walk and reused across monomorphizations
    // (move semantics don't depend on concrete type args).
    pub moved_places: Vec<MovedPlace>,
    // See FnSymbol.move_sites.
    pub move_sites: Vec<(crate::ast::NodeId, String)>,
}

pub struct MethodResolution {
    // For concrete methods (non-template), this is the WASM idx. For
    // generic-method calls, ignored — see `template_idx`/`type_args` instead.
    pub callee_idx: u32,
    pub callee_path: Vec<String>,
    pub recv_adjust: ReceiverAdjust,
    pub ret_borrows_receiver: bool,
    // When the method is a generic template (impl-generic and/or method-generic),
    // these record the resolution for codegen to monomorphize. type_args has
    // length = template's type_params.len(), in the same order: impl's params
    // first (bound to receiver type_args), then method's own (fresh vars
    // resolved by inference).
    pub template_idx: Option<usize>,
    pub type_args: Vec<RType>,
    // T2: deferred trait dispatch — populated when the call goes through
    // a `T: Trait` bound. Codegen substitutes `recv_type` against the
    // mono env and runs `solve_impl` to find the concrete impl + method.
    pub trait_dispatch: Option<TraitDispatch>,
}

pub struct TraitDispatch {
    pub trait_path: Vec<String>,
    pub method_name: String,
    pub recv_type: RType,
}

pub enum ReceiverAdjust {
    Move,        // recv is consumed; method takes Self
    BorrowImm,   // recv is owned; method takes &Self → emit &recv
    BorrowMut,   // recv is owned; method takes &mut Self → emit &mut recv
    ByRef,       // recv is &Self/&mut Self; pass i32 directly (incl. mut→imm downgrade)
}

pub struct FuncTable {
    pub entries: Vec<FnSymbol>,
    pub templates: Vec<GenericTemplate>,
}

pub fn template_lookup<'a>(
    table: &'a FuncTable,
    path: &Vec<String>,
) -> Option<(usize, &'a GenericTemplate)> {
    let mut i = 0;
    while i < table.templates.len() {
        if path_eq(&table.templates[i].path, path) {
            return Some((i, &table.templates[i]));
        }
        i += 1;
    }
    None
}

pub fn func_lookup<'a>(table: &'a FuncTable, path: &Vec<String>) -> Option<&'a FnSymbol> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

pub fn clone_path(path: &Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < path.len() {
        out.push(path[i].clone());
        i += 1;
    }
    out
}

pub fn path_eq(a: &Vec<String>, b: &Vec<String>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Resolve a path expression's segments to an absolute lookup path. Handles
// `Self::…` substitution: replaces a leading `Self` segment with the impl
// target's struct name. Used by both typeck and codegen for call and struct
// literal lookups.
pub fn resolve_full_path(
    current_module: &Vec<String>,
    self_target: Option<&RType>,
    segments: &Vec<PathSegment>,
) -> Vec<String> {
    let mut full = clone_path(current_module);
    let mut start = 0;
    if !segments.is_empty() && segments[0].name == "Self" {
        if let Some(RType::Struct { path: target_path, .. }) = self_target {
            if let Some(last) = target_path.last() {
                full.push(last.clone());
                start = 1;
            }
        }
    }
    let mut i = start;
    while i < segments.len() {
        full.push(segments[i].name.clone());
        i += 1;
    }
    full
}

pub fn segments_to_string(segs: &Vec<PathSegment>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < segs.len() {
        if i > 0 {
            s.push_str("::");
        }
        s.push_str(&segs[i].name);
        i += 1;
    }
    s
}

pub fn place_to_string(p: &Vec<String>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < p.len() {
        if i > 0 {
            s.push('.');
        }
        s.push_str(&p[i]);
        i += 1;
    }
    s
}

pub fn resolve_type(
    ty: &Type,
    current_module: &Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    self_target: Option<&RType>,
    type_params: &Vec<String>,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Result<RType, Error> {
    match &ty.kind {
        TypeKind::Path(path) => {
            if path.segments.len() == 1 {
                if path.segments[0].name == "bool" {
                    return Ok(RType::Bool);
                }
                if let Some(k) = int_kind_from_name(&path.segments[0].name) {
                    return Ok(RType::Int(k));
                }
                // Check if it's an in-scope type parameter.
                let name = &path.segments[0].name;
                let mut i = 0;
                while i < type_params.len() {
                    if type_params[i] == *name {
                        return Ok(RType::Param(name.clone()));
                    }
                    i += 1;
                }
            }
            // Try use-table resolution: probe for both struct and enum
            // entries (a use-imported name could be either).
            let raw_segs: Vec<String> =
                path.segments.iter().map(|s| s.name.clone()).collect();
            let mut full = if let Some(p) = resolve_via_use_scopes(
                &raw_segs,
                use_scope,
                |cand| {
                    struct_lookup_resolved(structs, reexports, cand).is_some()
                        || enum_lookup_resolved(enums, reexports, cand).is_some()
                },
            ) {
                p
            } else {
                let mut full = clone_path(current_module);
                let mut i = 0;
                while i < path.segments.len() {
                    full.push(path.segments[i].name.clone());
                    i += 1;
                }
                full
            };
            let last = &path.segments[path.segments.len() - 1];
            // Try enum first (so a name shared with a struct in different
            // modules picks the right one through use-scope resolution).
            // In practice struct/enum names live in disjoint namespaces
            // per module, so this is just a "look both places, take what
            // matches."
            if let Some(e_entry) = enum_lookup_resolved(enums, reexports, &full) {
                full = clone_path(&e_entry.path);
                if !is_visible_from(&type_defining_module(&e_entry.path), e_entry.is_pub, current_module) {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!("enum `{}` is private", place_to_string(&e_entry.path)),
                        span: path.span.copy(),
                    });
                }
                if e_entry.type_params.len() != last.args.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "wrong number of type arguments for `{}`: expected {}, got {}",
                            place_to_string(&full),
                            e_entry.type_params.len(),
                            last.args.len()
                        ),
                        span: path.span.copy(),
                    });
                }
                let lifetime_args = resolve_lifetime_args(
                    &last.lifetime_args,
                    &e_entry.lifetime_params,
                    &full,
                    file,
                    &path.span,
                )?;
                let mut type_args: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < last.args.len() {
                    type_args.push(resolve_type(
                        &last.args[i],
                        current_module,
                        structs,
                        enums,
                        self_target,
                        type_params,
                        use_scope,
                        reexports,
                        file,
                    )?);
                    i += 1;
                }
                return Ok(RType::Enum {
                    path: full,
                    type_args,
                    lifetime_args,
                });
            }
            let entry = match struct_lookup_resolved(structs, reexports, &full) {
                Some(e) => e,
                None => {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!("unknown type: {}", segments_to_string(&path.segments)),
                        span: path.span.copy(),
                    });
                }
            };
            // Use the canonical path returned by the resolver — that's
            // what downstream type representation expects (e.g.
            // `RType::Struct.path` should be the trait's actual
            // location, not the re-export alias).
            full = clone_path(&entry.path);
            if !is_visible_from(&type_defining_module(&entry.path), entry.is_pub, current_module) {
                return Err(Error {
                    file: file.to_string(),
                    message: format!("struct `{}` is private", place_to_string(&entry.path)),
                    span: path.span.copy(),
                });
            }
            if entry.type_params.len() != last.args.len() {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "wrong number of type arguments for `{}`: expected {}, got {}",
                        place_to_string(&full),
                        entry.type_params.len(),
                        last.args.len()
                    ),
                    span: path.span.copy(),
                });
            }
            let lifetime_args = resolve_lifetime_args(
                &last.lifetime_args,
                &entry.lifetime_params,
                &full,
                file,
                &path.span,
            )?;
            let mut type_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < last.args.len() {
                let t = resolve_type(
                    &last.args[i],
                    current_module,
                    structs,
                    enums,
                    self_target,
                    type_params,
                    use_scope,
                    reexports,
                    file,
                )?;
                type_args.push(t);
                i += 1;
            }
            Ok(RType::Struct {
                path: full,
                type_args,
                lifetime_args,
            })
        }
        TypeKind::Ref { inner, mutable, lifetime } => {
            let r = resolve_type(
                inner,
                current_module,
                structs,
                enums,
                self_target,
                type_params,
                use_scope,
                reexports,
                file,
            )?;
            // Phase B: structurally carry the lifetime — `'a` becomes
            // `Named("a")`; elided refs and the `'_` anonymous lifetime
            // both use the `Inferred(0)` placeholder, freshened later.
            let lt = match lifetime {
                Some(lt) if lt.name == "_" => LifetimeRepr::Inferred(0),
                Some(lt) => LifetimeRepr::Named(lt.name.clone()),
                None => LifetimeRepr::Inferred(0),
            };
            Ok(RType::Ref {
                inner: Box::new(r),
                mutable: *mutable,
                lifetime: lt,
            })
        }
        TypeKind::RawPtr { inner, mutable } => {
            let r = resolve_type(
                inner,
                current_module,
                structs,
                enums,
                self_target,
                type_params,
                use_scope,
                reexports,
                file,
            )?;
            Ok(RType::RawPtr {
                inner: Box::new(r),
                mutable: *mutable,
            })
        }
        TypeKind::SelfType => match self_target {
            Some(rt) => Ok(rtype_clone(rt)),
            None => Err(Error {
                file: file.to_string(),
                message: "`Self` is only valid inside an `impl` block".to_string(),
                span: ty.span.copy(),
            }),
        },
        TypeKind::Tuple(elems) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(resolve_type(
                    &elems[i],
                    current_module,
                    structs,
                    enums,
                    self_target,
                    type_params,
                    use_scope,
                    reexports,
                    file,
                )?);
                i += 1;
            }
            Ok(RType::Tuple(out))
        }
    }
}

// Validate and resolve the lifetime args at a struct/enum type-position
// path against the type's declared `lifetime_params`. Either fully
// elided (yields `Inferred(0)` placeholders, one per param) or a
// 1:1 explicit list (each `'a`-style name → `Named`, `'_` → fresh
// `Inferred(0)`). Used by both the struct and enum branches of
// `resolve_type` to share the validation.
fn resolve_lifetime_args(
    args: &Vec<crate::ast::Lifetime>,
    params: &Vec<String>,
    full: &Vec<String>,
    file: &str,
    span: &Span,
) -> Result<Vec<LifetimeRepr>, Error> {
    if args.is_empty() {
        let mut out: Vec<LifetimeRepr> = Vec::new();
        let mut i = 0;
        while i < params.len() {
            out.push(LifetimeRepr::Inferred(0));
            i += 1;
        }
        return Ok(out);
    }
    if args.len() != params.len() {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "wrong number of lifetime arguments for `{}`: expected {}, got {}",
                place_to_string(full),
                params.len(),
                args.len()
            ),
            span: span.copy(),
        });
    }
    let mut out: Vec<LifetimeRepr> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].name == "_" {
            out.push(LifetimeRepr::Inferred(0));
        } else {
            out.push(LifetimeRepr::Named(args[i].name.clone()));
        }
        i += 1;
    }
    Ok(out)
}

// Re-export-aware enum lookup. Mirrors `struct_lookup_resolved`.
pub fn enum_lookup_resolved<'a>(
    enums: &'a EnumTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<&'a EnumEntry> {
    if let Some(e) = enum_lookup(enums, path) {
        return Some(e);
    }
    let target = resolve_via_reexports(path, reexports, |cand| {
        enum_lookup(enums, cand).is_some()
    })?;
    enum_lookup(enums, &target)
}

// A path matches an enum variant if its prefix names an enum and the
// last segment matches one of that enum's variants. Returns the
// canonical enum path + variant index. The probe is use-scope and
// re-export aware — `Option::Some` resolves through `use std::option::Option`,
// `std::*` glob, `pub use`, etc.
pub fn lookup_variant_path(
    enums: &EnumTable,
    reexports: &ReExportTable,
    use_scope: &Vec<UseEntry>,
    current_module: &Vec<String>,
    raw_segs: &Vec<String>,
) -> Option<(Vec<String>, usize)> {
    if raw_segs.len() < 2 {
        return None;
    }
    let prefix_len = raw_segs.len() - 1;
    let variant_name = raw_segs[prefix_len].clone();
    let prefix_segs: Vec<String> = raw_segs[..prefix_len].to_vec();
    // Try use-scope resolution first; the prefix must name an enum.
    let enum_path: Vec<String> =
        resolve_via_use_scopes(&prefix_segs, use_scope, |cand| {
            enum_lookup_resolved(enums, reexports, cand).is_some()
        })
        .unwrap_or_else(|| {
            let mut full = clone_path(current_module);
            let mut i = 0;
            while i < prefix_segs.len() {
                full.push(prefix_segs[i].clone());
                i += 1;
            }
            full
        });
    let entry = enum_lookup_resolved(enums, reexports, &enum_path)?;
    let mut i = 0;
    while i < entry.variants.len() {
        if entry.variants[i].name == variant_name {
            return Some((clone_path(&entry.path), i));
        }
        i += 1;
    }
    None
}

// ----- Inference machinery -----

pub fn check(
    root: &Module,
    structs: &mut StructTable,
    enums: &mut EnumTable,
    traits: &mut TraitTable,
    funcs: &mut FuncTable,
    reexports: &mut ReExportTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    // Build this crate's re-export entries before any pass that does
    // path resolution, so `pub use` re-exports are visible to lookups.
    let crate_reexports = build_reexport_table(root);
    let mut k = 0;
    while k < crate_reexports.entries.len() {
        reexports.entries.push(crate_reexports.entries[k].clone());
        k += 1;
    }
    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_struct_names(root, &mut path, structs);

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_enum_names(root, &mut path, enums);

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_trait_names(root, &mut path, traits);

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_struct_fields(root, &mut path, structs, enums, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_enum_variants(root, &mut path, enums, structs, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_trait_methods(root, &mut path, traits, structs, enums, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_funcs(root, &mut path, funcs, next_idx, structs, enums, traits, reexports)?;

    validate_supertrait_obligations(traits)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    let mut current_file = root.source_file.clone();
    check_module(root, &mut path, &mut current_file, structs, enums, traits, funcs, reexports)?;

    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

// First-pass trait collection. Records each `trait Foo { fn ... ; }` with
// shell `TraitMethodEntry` placeholders (names + spans only). Full
// signature resolution happens in `resolve_trait_methods` after structs
// are resolved.
fn collect_trait_names(module: &Module, path: &mut Vec<String>, table: &mut TraitTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Trait(td) => {
                let mut full = clone_path(path);
                full.push(td.name.clone());
                let mut methods: Vec<TraitMethodEntry> = Vec::new();
                let mut k = 0;
                while k < td.methods.len() {
                    let type_params: Vec<String> = td.methods[k]
                        .type_params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect();
                    methods.push(TraitMethodEntry {
                        name: td.methods[k].name.clone(),
                        name_span: td.methods[k].name_span.copy(),
                        type_params,
                        param_types: Vec::new(),
                        return_type: None,
                        receiver_shape: None,
                    });
                    k += 1;
                }
                table.entries.push(TraitEntry {
                    path: full,
                    name_span: td.name_span.copy(),
                    file: module.source_file.clone(),
                    methods,
                    is_pub: td.is_pub,
                    supertraits: Vec::new(),
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_trait_names(m, path, table);
                path.pop();
            }
            Item::Function(_) => {}
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
}

// Second pass over trait declarations: resolve each method's full
// signature using `Self` as `RType::Param("Self")`, classify the
// receiver shape, and store back into `TraitTable.entries`. Runs after
// structs are resolved so method param/return types can reference user
// types.
fn resolve_trait_methods(
    module: &Module,
    path: &mut Vec<String>,
    traits: &mut TraitTable,
    structs: &StructTable,
    enums: &EnumTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = if path.is_empty() { "" } else { &path[0] };
    let use_scope = module_use_entries(module, crate_root);
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Trait(td) => {
                let mut full = clone_path(path);
                full.push(td.name.clone());
                // `Self` placeholder visible inside trait method sigs.
                let self_target = RType::Param("Self".to_string());
                // Find this trait's table entry index so we can mutate
                // its method list after resolving.
                let mut entry_idx: Option<usize> = None;
                let mut e = 0;
                while e < traits.entries.len() {
                    if path_eq(&traits.entries[e].path, &full) {
                        entry_idx = Some(e);
                        break;
                    }
                    e += 1;
                }
                let entry_idx = entry_idx.expect("trait registered above");
                let mut supertraits: Vec<Vec<String>> = Vec::new();
                let mut s = 0;
                while s < td.supertraits.len() {
                    let resolved = resolve_trait_path(
                        &td.supertraits[s].path,
                        path,
                        traits,
                        &use_scope,
                        reexports,
                        &module.source_file,
                    )?;
                    supertraits.push(resolved);
                    s += 1;
                }
                traits.entries[entry_idx].supertraits = supertraits;
                let mut k = 0;
                while k < td.methods.len() {
                    let m = &td.methods[k];
                    let type_params: Vec<String> =
                        m.type_params.iter().map(|p| p.name.clone()).collect();
                    let mut param_types: Vec<RType> = Vec::new();
                    let mut p = 0;
                    while p < m.params.len() {
                        let rt = resolve_type(
                            &m.params[p].ty,
                            path,
                            structs,
                            enums,
                            Some(&self_target),
                            &type_params,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        param_types.push(rt);
                        p += 1;
                    }
                    let return_type = match &m.return_type {
                        Some(ty) => Some(resolve_type(
                            ty,
                            path,
                            structs,
                            enums,
                            Some(&self_target),
                            &type_params,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?),
                        None => None,
                    };
                    let receiver_shape = if !m.params.is_empty() && m.params[0].name == "self" {
                        Some(classify_receiver_shape(&param_types[0]))
                    } else {
                        None
                    };
                    traits.entries[entry_idx].methods[k].param_types = param_types;
                    traits.entries[entry_idx].methods[k].return_type = return_type;
                    traits.entries[entry_idx].methods[k].receiver_shape = receiver_shape;
                    k += 1;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_trait_methods(m, path, traits, structs, enums, reexports)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
    Ok(())
}

fn classify_receiver_shape(rt: &RType) -> TraitReceiverShape {
    match rt {
        RType::Ref { mutable: true, .. } => TraitReceiverShape::BorrowMut,
        RType::Ref { mutable: false, .. } => TraitReceiverShape::BorrowImm,
        _ => TraitReceiverShape::Move,
    }
}

fn collect_struct_names(module: &Module, path: &mut Vec<String>, table: &mut StructTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = clone_path(path);
                full.push(sd.name.clone());
                let type_param_names: Vec<String> = sd
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let lifetime_param_names: Vec<String> = sd
                    .lifetime_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                table.entries.push(StructEntry {
                    path: full,
                    name_span: sd.name_span.copy(),
                    file: module.source_file.clone(),
                    type_params: type_param_names,
                    lifetime_params: lifetime_param_names,
                    fields: Vec::new(),
                    is_pub: sd.is_pub,
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_struct_names(m, path, table);
                path.pop();
            }
            Item::Function(_) => {}
            Item::Enum(_) => {}
            Item::Impl(_) => {}
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
}

// First-pass enum collection: register every `enum E { ... }` with shell
// variant entries (names + spans). Variant payload types are resolved
// later by `resolve_enum_variants`, after both struct and enum names
// are known so payloads can reference either.
fn collect_enum_names(module: &Module, path: &mut Vec<String>, table: &mut EnumTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Enum(ed) => {
                let mut full = clone_path(path);
                full.push(ed.name.clone());
                let type_param_names: Vec<String> = ed
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let lifetime_param_names: Vec<String> = ed
                    .lifetime_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let mut variants: Vec<EnumVariantEntry> = Vec::new();
                let mut k = 0;
                while k < ed.variants.len() {
                    variants.push(EnumVariantEntry {
                        name: ed.variants[k].name.clone(),
                        name_span: ed.variants[k].name_span.copy(),
                        disc: k as u32,
                        payload: VariantPayloadResolved::Unit,
                    });
                    k += 1;
                }
                table.entries.push(EnumEntry {
                    path: full,
                    name_span: ed.name_span.copy(),
                    file: module.source_file.clone(),
                    type_params: type_param_names,
                    lifetime_params: lifetime_param_names,
                    variants,
                    is_pub: ed.is_pub,
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_enum_names(m, path, table);
                path.pop();
            }
            Item::Function(_) => {}
            Item::Struct(_) => {}
            Item::Impl(_) => {}
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
}

// Second-pass: resolve each variant's payload types now that both struct
// and enum names are known. Mirrors `resolve_struct_fields`.
fn resolve_enum_variants(
    module: &Module,
    path: &mut Vec<String>,
    table: &mut EnumTable,
    structs: &StructTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = if path.is_empty() { "" } else { &path[0] };
    let use_scope = module_use_entries(module, crate_root);
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Enum(ed) => {
                let mut full = clone_path(path);
                full.push(ed.name.clone());
                let type_param_names: Vec<String> = ed
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let mut resolved: Vec<EnumVariantEntry> = Vec::new();
                let mut k = 0;
                while k < ed.variants.len() {
                    let v = &ed.variants[k];
                    let payload = match &v.payload {
                        crate::ast::VariantPayload::Unit => VariantPayloadResolved::Unit,
                        crate::ast::VariantPayload::Tuple(types) => {
                            let mut out: Vec<RType> = Vec::new();
                            let mut j = 0;
                            while j < types.len() {
                                let rt = resolve_type(
                                    &types[j],
                                    path,
                                    structs,
                                    table,
                                    None,
                                    &type_param_names,
                                    &use_scope,
                                    reexports,
                                    &module.source_file,
                                )?;
                                out.push(rt);
                                j += 1;
                            }
                            VariantPayloadResolved::Tuple(out)
                        }
                        crate::ast::VariantPayload::Struct(fields) => {
                            let mut out: Vec<RTypedField> = Vec::new();
                            let mut j = 0;
                            while j < fields.len() {
                                let rt = resolve_type(
                                    &fields[j].ty,
                                    path,
                                    structs,
                                    table,
                                    None,
                                    &type_param_names,
                                    &use_scope,
                                    reexports,
                                    &module.source_file,
                                )?;
                                out.push(RTypedField {
                                    name: fields[j].name.clone(),
                                    name_span: fields[j].name_span.copy(),
                                    ty: rt,
                                    is_pub: fields[j].is_pub,
                                });
                                j += 1;
                            }
                            VariantPayloadResolved::Struct(out)
                        }
                    };
                    resolved.push(EnumVariantEntry {
                        name: v.name.clone(),
                        name_span: v.name_span.copy(),
                        disc: k as u32,
                        payload,
                    });
                    k += 1;
                }
                let entry_idx = {
                    let mut idx: Option<usize> = None;
                    let mut e = 0;
                    while e < table.entries.len() {
                        if path_eq(&table.entries[e].path, &full) {
                            idx = Some(e);
                            break;
                        }
                        e += 1;
                    }
                    idx.expect("enum registered in collect_enum_names")
                };
                table.entries[entry_idx].variants = resolved;
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_enum_variants(m, path, table, structs, reexports)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Struct(_) => {}
            Item::Impl(_) => {}
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
    Ok(())
}

fn resolve_struct_fields(
    module: &Module,
    path: &mut Vec<String>,
    table: &mut StructTable,
    enums: &EnumTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = clone_path(path);
                full.push(sd.name.clone());
                let type_param_names: Vec<String> = sd
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let lifetime_param_names: Vec<String> = sd
                    .lifetime_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let mut resolved: Vec<RTypedField> = Vec::new();
                let crate_root: &str = if path.is_empty() { "" } else { &path[0] };
    let use_scope = module_use_entries(module, crate_root);
                let mut k = 0;
                while k < sd.fields.len() {
                    let rt = resolve_type(
                        &sd.fields[k].ty,
                        path,
                        table,
                        enums,
                        None,
                        &type_param_names,
                        &use_scope,
                        reexports,
                        &module.source_file,
                    )?;
                    // Phase D: refs are allowed in struct fields. Their
                    // lifetimes must be `Named` and declared in the struct's
                    // `<'a, ...>` params — elided refs in field types aren't
                    // permitted (Rust requires explicit lifetimes there too).
                    require_no_inferred_lifetimes(
                        &rt,
                        &sd.fields[k].ty.span,
                        &module.source_file,
                    )?;
                    validate_named_lifetimes(
                        &rt,
                        &lifetime_param_names,
                        &sd.fields[k].ty.span,
                        &module.source_file,
                    )?;
                    resolved.push(RTypedField {
                        name: sd.fields[k].name.clone(),
                        name_span: sd.fields[k].name_span.copy(),
                        ty: rt,
                        is_pub: sd.fields[k].is_pub,
                    });
                    k += 1;
                }
                let mut e = 0;
                while e < table.entries.len() {
                    if path_eq(&table.entries[e].path, &full) {
                        table.entries[e].fields = resolved;
                        break;
                    }
                    e += 1;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_struct_fields(m, path, table, enums, reexports)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Enum(_) => {}
            Item::Impl(_) => {}
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
    Ok(())
}

fn collect_funcs(
    module: &Module,
    path: &mut Vec<String>,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &mut TraitTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = if path.is_empty() { "" } else { &path[0] };
    let use_scope = module_use_entries(module, crate_root);
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                register_function(
                    f,
                    path,
                    path,
                    None,
                    &Vec::new(),
                    &Vec::new(),
                    &Vec::new(),
                    None,
                    funcs,
                    next_idx,
                    structs,
                    enums,
                    traits,
                    &use_scope,
                    reexports,
                    &module.source_file,
                )?;
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_funcs(m, path, funcs, next_idx, structs, enums, traits, reexports)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(ib) => {
                let target_rt = resolve_impl_target(ib, path, structs, enums, &use_scope, reexports, &module.source_file)?;
                let impl_type_params: Vec<String> =
                    ib.type_params.iter().map(|p| p.name.clone()).collect();
                let impl_lifetime_params: Vec<String> =
                    ib.lifetime_params.iter().map(|p| p.name.clone()).collect();
                // Resolve the impl's type-param bounds eagerly (used by both
                // signature checks and bound enforcement in body checking).
                let mut impl_type_param_bounds: Vec<Vec<Vec<String>>> = Vec::new();
                let mut bi = 0;
                while bi < ib.type_params.len() {
                    let mut row: Vec<Vec<String>> = Vec::new();
                    let mut bj = 0;
                    while bj < ib.type_params[bi].bounds.len() {
                        let resolved = resolve_trait_path(
                            &ib.type_params[bi].bounds[bj].path,
                            path,
                            traits,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        row.push(resolved);
                        bj += 1;
                    }
                    impl_type_param_bounds.push(row);
                    bi += 1;
                }
                let trait_impl_idx_for_methods: Option<usize> =
                    if let Some(trait_path_node) = &ib.trait_path {
                        let trait_full = resolve_trait_path(
                            trait_path_node,
                            path,
                            traits,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        validate_trait_impl(
                            ib,
                            &trait_full,
                            traits,
                            &module.source_file,
                        )?;
                        let idx = traits.impls.len();
                        register_trait_impl(
                            ib,
                            &trait_full,
                            rtype_clone(&target_rt),
                            &impl_type_params,
                            &impl_lifetime_params,
                            &impl_type_param_bounds,
                            traits,
                            &module.source_file,
                        )?;
                        // T2.5: `impl Copy for SomeStruct {}` (concrete or
                        // generic) requires every field's type to be Copy.
                        // Generic impls use the impl-type-param bounds, so
                        // `impl<T: Copy> Copy for Wrap<T> {}` works.
                        if path_eq(&trait_full, &copy_trait_path()) {
                            validate_copy_impl(
                                &target_rt,
                                &impl_type_params,
                                &impl_type_param_bounds,
                                structs,
                                traits,
                                &ib.span,
                                &module.source_file,
                            )?;
                        }
                        Some(idx)
                    } else {
                        None
                    };
                // Method-path prefix. Mirror codegen's derivation: take the
                // first segment of the target's AST Path. For non-Path
                // targets (e.g. `&T`), synthesize a unique slot.
                let target_name_for_prefix: Option<String> = match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if !p.segments.is_empty() => {
                        Some(p.segments[0].name.clone())
                    }
                    _ => None,
                };
                let mut method_prefix = clone_path(path);
                if let Some(name) = &target_name_for_prefix {
                    method_prefix.push(name.clone());
                } else {
                    // Trait impl on a non-struct (e.g. `&T`): synthesize a
                    // unique prefix.
                    method_prefix.push(format!(
                        "__trait_impl_{}",
                        trait_impl_idx_for_methods.unwrap_or(0)
                    ));
                }
                let mut k = 0;
                while k < ib.methods.len() {
                    register_function(
                        &ib.methods[k],
                        path,
                        &method_prefix,
                        Some(&target_rt),
                        &impl_type_params,
                        &impl_lifetime_params,
                        &impl_type_param_bounds,
                        trait_impl_idx_for_methods,
                        funcs,
                        next_idx,
                        structs,
                        enums,
                        traits,
                        &use_scope,
                        reexports,
                        &module.source_file,
                    )?;
                    k += 1;
                }
                // T2.5: validate impl method signatures against the trait
                // declaration (for trait impls only).
                if let Some(trait_path_node) = &ib.trait_path {
                    let trait_full = resolve_trait_path(
                        trait_path_node,
                        path,
                        traits,
                        &use_scope,
                        reexports,
                        &module.source_file,
                    )?;
                    validate_trait_impl_signatures(
                        ib,
                        &trait_full,
                        &target_rt,
                        &method_prefix,
                        funcs,
                        traits,
                        &module.source_file,
                    )?;
                }
            }
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
    Ok(())
}

// T2.6: find the `traits.impls` row corresponding to a given AST impl
// block (matching by trait_path + target_rtype_eq). Returns None for
// inherent impls (no trait_path) — those don't have a row.
fn find_trait_impl_idx(
    ib: &crate::ast::ImplBlock,
    target_rt: &RType,
    current_module: &Vec<String>,
    traits: &TraitTable,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Option<usize> {
    let trait_path_node = ib.trait_path.as_ref()?;
    let trait_full = match resolve_trait_path(trait_path_node, current_module, traits, use_scope, reexports, file) {
        Ok(p) => p,
        Err(_) => return None,
    };
    let mut i = 0;
    while i < traits.impls.len() {
        if path_eq(&traits.impls[i].trait_path, &trait_full)
            && rtype_eq(&traits.impls[i].target, target_rt)
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

// Resolve a `trait_path` (as written in `impl Trait for ...`) to a
// concrete trait entry path. Lookup order:
// 1. `current_module ++ path` (the relative form).
// 2. `path` taken absolute (covers `std::Copy` written in user code).
// 3. If the user wrote a single segment `T`, search for any registered
//    trait whose path ends with that segment — approximates a prelude
//    so `Copy` resolves to `std::Copy`. Multiple matches → ambiguity.
fn resolve_trait_path(
    p: &crate::ast::Path,
    current_module: &Vec<String>,
    traits: &TraitTable,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Result<Vec<String>, Error> {
    let raw_segs: Vec<String> = p.segments.iter().map(|s| s.name.clone()).collect();
    let attempts: Vec<Vec<String>> = {
        let mut v: Vec<Vec<String>> = Vec::new();
        // Use-table (re-export-aware probe).
        if let Some(via_use) = resolve_via_use_scopes(
            &raw_segs,
            use_scope,
            |cand| trait_lookup_resolved(traits, reexports, cand).is_some(),
        ) {
            v.push(via_use);
        }
        // Module-relative.
        let mut full = clone_path(current_module);
        let mut i = 0;
        while i < p.segments.len() {
            full.push(p.segments[i].name.clone());
            i += 1;
        }
        v.push(full);
        // Absolute (no current-module prefix).
        let mut alt: Vec<String> = Vec::new();
        let mut i = 0;
        while i < p.segments.len() {
            alt.push(p.segments[i].name.clone());
            i += 1;
        }
        v.push(alt);
        v
    };
    let mut k = 0;
    while k < attempts.len() {
        if let Some(entry) = trait_lookup_resolved(traits, reexports, &attempts[k]) {
            if !is_visible_from(&type_defining_module(&entry.path), entry.is_pub, current_module) {
                return Err(Error {
                    file: file.to_string(),
                    message: format!("trait `{}` is private", place_to_string(&entry.path)),
                    span: p.span.copy(),
                });
            }
            // Return the canonical path so downstream lookups (impls,
            // method dispatch) all key off the trait's real location.
            return Ok(clone_path(&entry.path));
        }
        k += 1;
    }
    Err(Error {
        file: file.to_string(),
        message: format!("unknown trait: {}", segments_to_string(&p.segments)),
        span: p.span.copy(),
    })
}

// Validate that `ib` (with `trait_path` Some) covers exactly the trait's
// methods — every trait method has an impl method by name, and there are
// no extra methods that the trait doesn't declare. Method-signature
// equality is left for T2.
fn validate_trait_impl(
    ib: &crate::ast::ImplBlock,
    trait_full: &Vec<String>,
    traits: &TraitTable,
    file: &str,
) -> Result<(), Error> {
    let entry = trait_lookup(traits, trait_full).expect("resolved above");
    // Every trait method must be implemented.
    let mut t = 0;
    while t < entry.methods.len() {
        let trait_method_name = &entry.methods[t].name;
        let mut found = false;
        let mut k = 0;
        while k < ib.methods.len() {
            if &ib.methods[k].name == trait_method_name {
                found = true;
                break;
            }
            k += 1;
        }
        if !found {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "missing trait method `{}` in impl of `{}`",
                    trait_method_name,
                    place_to_string(trait_full)
                ),
                span: ib.span.copy(),
            });
        }
        t += 1;
    }
    // No extra methods.
    let mut k = 0;
    while k < ib.methods.len() {
        let m_name = &ib.methods[k].name;
        let mut t = 0;
        let mut declared = false;
        while t < entry.methods.len() {
            if entry.methods[t].name == *m_name {
                declared = true;
                break;
            }
            t += 1;
        }
        if !declared {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "method `{}` is not a member of trait `{}`",
                    m_name,
                    place_to_string(trait_full)
                ),
                span: ib.methods[k].name_span.copy(),
            });
        }
        k += 1;
    }
    Ok(())
}

// T2.5 + T2.5b: signature equality between impl methods and the trait
// declaration. Substitutes `Self → impl_target` in the trait method's
// signature and compares param types + return type via `rtype_eq`.
// When the trait method has its own type-params (`fn bar<U>(...)`), the
// comparison treats trait's `U_i` and impl's `V_i` as α-equivalent —
// both substituted to a shared placeholder `Param("__trait_method_i")`
// before `rtype_eq`. Arities (param count and method-level type-param
// count) must match. Receivers are not dropped from the comparison; the
// trait's receiver shape is also enforced separately.
fn validate_trait_impl_signatures(
    ib: &crate::ast::ImplBlock,
    trait_full: &Vec<String>,
    target_rt: &RType,
    method_prefix: &Vec<String>,
    funcs: &FuncTable,
    traits: &TraitTable,
    file: &str,
) -> Result<(), Error> {
    let trait_entry = match trait_lookup(traits, trait_full) {
        Some(e) => e,
        None => return Ok(()),
    };
    let mut k = 0;
    while k < ib.methods.len() {
        let m_name = &ib.methods[k].name;
        // Find the matching trait method (validated to exist by
        // validate_trait_impl earlier).
        let mut tm_idx: Option<usize> = None;
        let mut t = 0;
        while t < trait_entry.methods.len() {
            if trait_entry.methods[t].name == *m_name {
                tm_idx = Some(t);
                break;
            }
            t += 1;
        }
        let tm_idx = match tm_idx {
            Some(v) => v,
            None => {
                k += 1;
                continue;
            }
        };
        let trait_method = &trait_entry.methods[tm_idx];
        // Arity check on method-level type-params (`<U>` in trait vs `<V>`
        // in impl). The trait's count is canonical; the impl must match.
        if trait_method.type_params.len() != ib.methods[k].type_params.len() {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "method `{}` has wrong number of type parameters: trait declares {}, impl has {}",
                    m_name,
                    trait_method.type_params.len(),
                    ib.methods[k].type_params.len()
                ),
                span: ib.methods[k].name_span.copy(),
            });
        }
        // Look up the registered impl method's signature.
        let mut full = clone_path(method_prefix);
        full.push(m_name.clone());
        let (impl_param_types, impl_return_type) =
            if let Some(entry) = func_lookup(funcs, &full) {
                (
                    rtype_vec_clone(&entry.param_types),
                    entry.return_type.as_ref().map(rtype_clone),
                )
            } else if let Some((_, t)) = template_lookup(funcs, &full) {
                (
                    rtype_vec_clone(&t.param_types),
                    t.return_type.as_ref().map(rtype_clone),
                )
            } else {
                k += 1;
                continue;
            };
        // Build α-equivalence environments. Trait side: `Self →
        // impl_target`, plus each `U_i → Param("__trait_method_<i>")`.
        // Impl side: each `V_i → Param("__trait_method_<i>")`. The
        // shared placeholder makes the two signatures comparable via
        // plain `rtype_eq` once both are substituted.
        let mut trait_env: Vec<(String, RType)> =
            vec![("Self".to_string(), rtype_clone(target_rt))];
        let mut impl_env: Vec<(String, RType)> = Vec::new();
        let mut tp = 0;
        while tp < trait_method.type_params.len() {
            let placeholder = format!("__trait_method_{}", tp);
            trait_env.push((
                trait_method.type_params[tp].clone(),
                RType::Param(placeholder.clone()),
            ));
            impl_env.push((
                ib.methods[k].type_params[tp].name.clone(),
                RType::Param(placeholder),
            ));
            tp += 1;
        }
        let mut expected_param_types: Vec<RType> = Vec::new();
        let mut p = 0;
        while p < trait_method.param_types.len() {
            expected_param_types.push(substitute_rtype(&trait_method.param_types[p], &trait_env));
            p += 1;
        }
        let expected_return_type: Option<RType> = trait_method
            .return_type
            .as_ref()
            .map(|rt| substitute_rtype(rt, &trait_env));
        // Substitute the impl method's signature too, so its `<V>`
        // params land on the same placeholders as the trait's `<U>`.
        let mut impl_param_types_sub: Vec<RType> = Vec::new();
        let mut p = 0;
        while p < impl_param_types.len() {
            impl_param_types_sub.push(substitute_rtype(&impl_param_types[p], &impl_env));
            p += 1;
        }
        let impl_return_type_sub: Option<RType> = impl_return_type
            .as_ref()
            .map(|rt| substitute_rtype(rt, &impl_env));
        let impl_param_types = impl_param_types_sub;
        let impl_return_type = impl_return_type_sub;
        // Compare arities + each param type.
        if expected_param_types.len() != impl_param_types.len() {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "method `{}` has wrong number of parameters: trait declares {}, impl has {}",
                    m_name,
                    expected_param_types.len(),
                    impl_param_types.len()
                ),
                span: ib.methods[k].name_span.copy(),
            });
        }
        let mut p = 0;
        while p < expected_param_types.len() {
            if !rtype_eq(&expected_param_types[p], &impl_param_types[p]) {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "method `{}` has wrong parameter type at position {}: trait declares `{}`, impl has `{}`",
                        m_name,
                        p,
                        rtype_to_string(&expected_param_types[p]),
                        rtype_to_string(&impl_param_types[p])
                    ),
                    span: ib.methods[k].name_span.copy(),
                });
            }
            p += 1;
        }
        match (&expected_return_type, &impl_return_type) {
            (Some(e), Some(a)) => {
                if !rtype_eq(e, a) {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "method `{}` has wrong return type: trait declares `{}`, impl has `{}`",
                            m_name,
                            rtype_to_string(e),
                            rtype_to_string(a)
                        ),
                        span: ib.methods[k].name_span.copy(),
                    });
                }
            }
            (None, None) => {}
            (Some(e), None) => {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "method `{}` is missing a return type (trait declares `{}`)",
                        m_name,
                        rtype_to_string(e)
                    ),
                    span: ib.methods[k].name_span.copy(),
                });
            }
            (None, Some(a)) => {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "method `{}` has return type `{}` but trait declares no return",
                        m_name,
                        rtype_to_string(a)
                    ),
                    span: ib.methods[k].name_span.copy(),
                });
            }
        }
        k += 1;
    }
    Ok(())
}

// T3/T2.5: validates that `impl Copy for Target {}` is well-formed. For
// struct targets, walks fields and rejects any non-Copy field type
// (after substituting impl-type-params). For generic impls like
// `impl<T: Copy> Copy for Wrap<T> {}`, the bound `T: Copy` makes
// `Param(T)` Copy, so `Wrap<T>` is admitted. Without that bound, a
// `Param(T)` field is rejected.
fn validate_copy_impl(
    target: &RType,
    impl_type_params: &Vec<String>,
    impl_type_param_bounds: &Vec<Vec<Vec<String>>>,
    structs: &StructTable,
    traits: &TraitTable,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    let (struct_path, type_args) = match target {
        RType::Struct { path, type_args, .. } => (path, type_args),
        _ => return Ok(()),
    };
    let entry = match struct_lookup(structs, struct_path) {
        Some(e) => e,
        None => return Ok(()),
    };
    let env = struct_env(&entry.type_params, type_args);
    let mut i = 0;
    while i < entry.fields.len() {
        let field_ty = substitute_rtype(&entry.fields[i].ty, &env);
        if !is_copy_with_bounds(&field_ty, traits, impl_type_params, impl_type_param_bounds) {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "the trait `Copy` is not implemented for `{}`: field `{}` has type `{}`",
                    rtype_to_string(target),
                    entry.fields[i].name,
                    rtype_to_string(&field_ty)
                ),
                span: span.copy(),
            });
        }
        i += 1;
    }
    Ok(())
}

// Register a `(trait_path, target_pattern)` row. Rejects exact-pattern
// duplicates: a second `impl T for Pat` where Pat's RType is `rtype_eq`
// to a prior one.
fn register_trait_impl(
    ib: &crate::ast::ImplBlock,
    trait_full: &Vec<String>,
    target: RType,
    impl_type_params: &Vec<String>,
    impl_lifetime_params: &Vec<String>,
    impl_type_param_bounds: &Vec<Vec<Vec<String>>>,
    traits: &mut TraitTable,
    file: &str,
) -> Result<(), Error> {
    let mut i = 0;
    while i < traits.impls.len() {
        if path_eq(&traits.impls[i].trait_path, trait_full)
            && rtype_eq(&traits.impls[i].target, &target)
        {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "duplicate impl of trait `{}` for `{}`",
                    place_to_string(trait_full),
                    rtype_to_string(&target)
                ),
                span: ib.span.copy(),
            });
        }
        i += 1;
    }
    // T4: Drop and Copy are mutually exclusive. If we're registering one,
    // refuse if the other is already implemented for this exact target.
    let copy_path = copy_trait_path();
    let drop_path = drop_trait_path();
    let conflicting_path: Option<Vec<String>> = if path_eq(trait_full, &copy_path) {
        Some(drop_path.clone())
    } else if path_eq(trait_full, &drop_path) {
        Some(copy_path.clone())
    } else {
        None
    };
    if let Some(other) = conflicting_path {
        let mut i = 0;
        while i < traits.impls.len() {
            if path_eq(&traits.impls[i].trait_path, &other)
                && rtype_eq(&traits.impls[i].target, &target)
            {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "the trait `{}` cannot be implemented for `{}` because it already implements `{}`",
                        place_to_string(trait_full),
                        rtype_to_string(&target),
                        place_to_string(&other)
                    ),
                    span: ib.span.copy(),
                });
            }
            i += 1;
        }
    }
    let mut bounds_clone: Vec<Vec<Vec<String>>> = Vec::new();
    let mut i = 0;
    while i < impl_type_param_bounds.len() {
        let mut row: Vec<Vec<String>> = Vec::new();
        let mut j = 0;
        while j < impl_type_param_bounds[i].len() {
            row.push(impl_type_param_bounds[i][j].clone());
            j += 1;
        }
        bounds_clone.push(row);
        i += 1;
    }
    traits.impls.push(TraitImplEntry {
        trait_path: clone_path(trait_full),
        target,
        impl_type_params: impl_type_params.clone(),
        impl_lifetime_params: impl_lifetime_params.clone(),
        impl_type_param_bounds: bounds_clone,
        file: file.to_string(),
        span: ib.span.copy(),
    });
    Ok(())
}

// Walks every registered `impl Trait for T` and verifies that for each
// supertrait `S` of `Trait`, there is also an `impl S for T`. Done after
// all impls are registered (in any source order). The impl-target may be
// a generic pattern with `Param(name)` slots; supertrait checks consult
// the impl's own type-param bounds via `solve_impl_in_ctx` so that
// `impl<T: PartialEq> Eq for Wrap<T>` is satisfied by the generic
// `impl<T: PartialEq> PartialEq for Wrap<T>` row.
fn validate_supertrait_obligations(traits: &TraitTable) -> Result<(), Error> {
    let mut i = 0;
    while i < traits.impls.len() {
        let row = &traits.impls[i];
        let entry = match trait_lookup(traits, &row.trait_path) {
            Some(e) => e,
            None => {
                i += 1;
                continue;
            }
        };
        let mut s = 0;
        while s < entry.supertraits.len() {
            let sup = &entry.supertraits[s];
            if solve_impl_in_ctx(
                sup,
                &row.target,
                traits,
                &row.impl_type_params,
                &row.impl_type_param_bounds,
                0,
            )
            .is_none()
            {
                return Err(Error {
                    file: row.file.clone(),
                    message: format!(
                        "the trait bound `{}: {}` is not satisfied (required by `{}`)",
                        rtype_to_string(&row.target),
                        place_to_string(sup),
                        place_to_string(&row.trait_path)
                    ),
                    span: row.span.copy(),
                });
            }
            s += 1;
        }
        i += 1;
    }
    Ok(())
}

// Resolves an `impl Path { ... }` target to its struct type. The impl's type
// params must match the target struct's type params 1:1 (e.g., `impl<T, U>
// Pair<T, U>`). Returns the struct type with `Param(...)` type args matching
// the impl's parameter names.
fn resolve_impl_target(
    ib: &crate::ast::ImplBlock,
    current_module: &Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Result<RType, Error> {
    // Impl target is a full Type pattern with the impl's type-params in
    // scope. For inherent impls (no trait_path) this still must resolve to
    // a struct (since we can't add inherent methods to refs/raw-ptrs); for
    // trait impls any pattern is allowed (`impl Show for &T`, etc.).
    let impl_param_names: Vec<String> = ib.type_params.iter().map(|p| p.name.clone()).collect();
    let resolved = resolve_type(
        &ib.target,
        current_module,
        structs,
        enums,
        None,
        &impl_param_names,
        use_scope,
        reexports,
        file,
    )?;
    if ib.trait_path.is_none() {
        // Inherent: must be a struct.
        match &resolved {
            RType::Struct { .. } => {}
            _ => {
                return Err(Error {
                    file: file.to_string(),
                    message: "inherent impl target must be a struct".to_string(),
                    span: ib.target.span.copy(),
                });
            }
        }
    }
    Ok(resolved)
}

fn register_function(
    f: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    impl_type_params: &Vec<String>,
    impl_lifetime_params: &Vec<String>,
    impl_type_param_bounds: &Vec<Vec<Vec<String>>>,
    trait_impl_idx: Option<usize>,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    source_file: &str,
) -> Result<(), Error> {
    let mut type_param_names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < impl_type_params.len() {
        type_param_names.push(impl_type_params[i].clone());
        i += 1;
    }
    let mut i = 0;
    while i < f.type_params.len() {
        type_param_names.push(f.type_params[i].name.clone());
        i += 1;
    }
    // Lifetime params in scope: impl's then this fn's.
    let mut lifetime_param_names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < impl_lifetime_params.len() {
        lifetime_param_names.push(impl_lifetime_params[i].clone());
        i += 1;
    }
    let mut i = 0;
    while i < f.lifetime_params.len() {
        lifetime_param_names.push(f.lifetime_params[i].name.clone());
        i += 1;
    }
    let is_generic = !type_param_names.is_empty();
    let mut full = clone_path(path_prefix);
    full.push(f.name.clone());
    let mut param_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < f.params.len() {
        let rt = resolve_type(
            &f.params[k].ty,
            current_module,
            structs,
            enums,
            self_target,
            &type_param_names,
            use_scope,
            reexports,
            source_file,
        )?;
        param_types.push(rt);
        k += 1;
    }
    let mut return_type = match &f.return_type {
        Some(ty) => Some(resolve_type(
            ty,
            current_module,
            structs,
            enums,
            self_target,
            &type_param_names,
            use_scope,
            reexports,
            source_file,
        )?),
        None => None,
    };
    // Per-function fresh-id counter for elided lifetimes. 0 stays as the
    // "placeholder pre-resolution" sentinel; real ids start at 1.
    let mut next_lt: u32 = 1;
    let mut k = 0;
    while k < param_types.len() {
        freshen_inferred_lifetimes(&mut param_types[k], &mut next_lt);
        k += 1;
    }
    // Validate Named lifetimes in params now that they're fully shaped.
    let mut k = 0;
    while k < param_types.len() {
        validate_named_lifetimes(
            &param_types[k],
            &lifetime_param_names,
            &f.params[k].ty.span,
            source_file,
        )?;
        k += 1;
    }
    // For the return type: freshen INNER refs, then handle the outermost
    // lifetime via elision (if the outer is `Inferred(0)`, i.e. user wrote
    // an elided ref). A user-written `&'a T` outermost is already `Named`.
    let self_idx = if !f.params.is_empty() && f.params[0].name == "self" {
        Some(0)
    } else {
        None
    };
    if let (Some(rt), Some(ret_ty)) = (return_type.as_mut(), f.return_type.as_ref()) {
        // Freshen inner refs first (skip outermost if rt is itself a ref).
        match &mut *rt {
            RType::Ref { inner, .. } => {
                freshen_inferred_lifetimes(inner, &mut next_lt);
            }
            other => freshen_inferred_lifetimes(other, &mut next_lt),
        }
        // Apply elision / lifetime tying for the outermost ref.
        if let RType::Ref {
            mutable: ret_mut,
            lifetime: ret_lt,
            ..
        } = &mut *rt
        {
            let need_elision = matches!(ret_lt, LifetimeRepr::Inferred(0));
            if need_elision {
                let src_idx = find_elision_source(
                    &param_types,
                    self_idx,
                    *ret_mut,
                    &ret_ty.span,
                    source_file,
                )?;
                let src_lt =
                    outer_lifetime(&param_types[src_idx]).expect("elision source is a ref");
                *ret_lt = src_lt;
            }
        }
        // Validate Named lifetimes in the return type (including the outer
        // one if user-written).
        validate_named_lifetimes(rt, &lifetime_param_names, &ret_ty.span, source_file)?;
    }
    // Compute param_lifetimes / ret_lifetime now that signature is final.
    let mut param_lifetimes: Vec<Option<LifetimeRepr>> = Vec::new();
    let mut k = 0;
    while k < param_types.len() {
        param_lifetimes.push(outer_lifetime(&param_types[k]));
        k += 1;
    }
    let ret_lifetime: Option<LifetimeRepr> = match &return_type {
        Some(rt) => outer_lifetime(rt),
        None => None,
    };
    let impl_target_for_storage: Option<RType> = self_target.map(rtype_clone);
    // Combine impl-level + fn-level type-param bounds in the same order
    // as `type_param_names`.
    let mut type_param_bounds: Vec<Vec<Vec<String>>> = Vec::new();
    let mut i = 0;
    while i < impl_type_param_bounds.len() {
        let mut row: Vec<Vec<String>> = Vec::new();
        let mut j = 0;
        while j < impl_type_param_bounds[i].len() {
            row.push(impl_type_param_bounds[i][j].clone());
            j += 1;
        }
        type_param_bounds.push(row);
        i += 1;
    }
    let mut i = 0;
    while i < f.type_params.len() {
        let mut row: Vec<Vec<String>> = Vec::new();
        let mut j = 0;
        while j < f.type_params[i].bounds.len() {
            let resolved = resolve_trait_path(
                &f.type_params[i].bounds[j].path,
                current_module,
                traits,
                use_scope,
                reexports,
                source_file,
            )?;
            row.push(resolved);
            j += 1;
        }
        type_param_bounds.push(row);
        i += 1;
    }
    if is_generic {
        funcs.templates.push(GenericTemplate {
            path: full,
            type_params: type_param_names,
            type_param_bounds,
            impl_type_param_count: impl_type_params.len(),
            func: f.clone(),
            enclosing_module: clone_path(current_module),
            source_file: source_file.to_string(),
            param_types,
            return_type,
            expr_types: Vec::new(),
            param_lifetimes,
            ret_lifetime,
            impl_target: impl_target_for_storage,
            trait_impl_idx,
            is_pub: f.is_pub,
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
            moved_places: Vec::new(),
            move_sites: Vec::new(),
        });
    } else {
        funcs.entries.push(FnSymbol {
            path: full,
            idx: *next_idx,
            param_types,
            return_type,
            expr_types: Vec::new(),
            param_lifetimes,
            ret_lifetime,
            impl_target: impl_target_for_storage,
            trait_impl_idx,
            is_pub: f.is_pub,
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
            moved_places: Vec::new(),
            move_sites: Vec::new(),
        });
        *next_idx += 1;
    }
    Ok(())
}

// Lifetime elision for an elided return ref. Rule 3: when a method has
// `&self` / `&mut self`, the output borrow's lifetime is `self`'s,
// regardless of other ref params. Rule 2: otherwise, exactly one input ref
// param → its lifetime. `&mut T -> &U` is allowed (downgrade); `&T -> &mut U`
// is rejected. Returns the source param index; the caller copies that
// param's outermost lifetime into the return ref.
fn find_elision_source(
    param_types: &Vec<RType>,
    self_idx: Option<usize>,
    ret_mutable: bool,
    ret_span: &Span,
    file: &str,
) -> Result<usize, Error> {
    // Rule 3: a self-receiver that's a ref shorts the search.
    if let Some(idx) = self_idx {
        if let RType::Ref {
            mutable: src_mutable,
            ..
        } = &param_types[idx]
        {
            if ret_mutable && !*src_mutable {
                return Err(Error {
                    file: file.to_string(),
                    message: "cannot return `&mut` from a `&self` receiver".to_string(),
                    span: ret_span.copy(),
                });
            }
            return Ok(idx);
        }
        // Self is owned (consuming) — fall through to rule 2.
    }
    let mut source: Option<usize> = None;
    let mut count: usize = 0;
    let mut i = 0;
    while i < param_types.len() {
        if let RType::Ref { .. } = &param_types[i] {
            count += 1;
            source = Some(i);
        }
        i += 1;
    }
    if count != 1 {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "function returning a reference must have exactly one reference parameter (found {})",
                count
            ),
            span: ret_span.copy(),
        });
    }
    let src_idx = source.expect("count == 1");
    let src_mutable = match &param_types[src_idx] {
        RType::Ref { mutable, .. } => *mutable,
        _ => unreachable!(),
    };
    if ret_mutable && !src_mutable {
        return Err(Error {
            file: file.to_string(),
            message: "cannot return `&mut` from a `&` parameter".to_string(),
            span: ret_span.copy(),
        });
    }
    Ok(src_idx)
}

// ----- InferType -----

enum InferType {
    Var(u32),
    Int(IntKind),
    Struct {
        path: Vec<String>,
        type_args: Vec<InferType>,
        // Mirrors `RType::Struct.lifetime_args`. Carry-only for inference;
        // unification ignores lifetimes (Phase D structural).
        lifetime_args: Vec<LifetimeRepr>,
    },
    Ref {
        inner: Box<InferType>,
        mutable: bool,
        // Phase B: structural carry only. Mirrors `RType::Ref.lifetime`;
        // unification ignores it.
        lifetime: LifetimeRepr,
    },
    RawPtr { inner: Box<InferType>, mutable: bool },
    Param(String),
    Bool,
    Tuple(Vec<InferType>),
    Enum {
        path: Vec<String>,
        type_args: Vec<InferType>,
        lifetime_args: Vec<LifetimeRepr>,
    },
}

fn infer_clone(t: &InferType) -> InferType {
    match t {
        InferType::Var(v) => InferType::Var(*v),
        InferType::Int(k) => InferType::Int(int_kind_copy(k)),
        InferType::Struct { path, type_args, lifetime_args } => InferType::Struct {
            path: clone_path(path),
            type_args: infer_vec_clone(type_args),
            lifetime_args: lifetime_repr_vec_clone(lifetime_args),
        },
        InferType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(infer_clone(inner)),
            mutable: *mutable,
            lifetime: lifetime_repr_clone(lifetime),
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_clone(inner)),
            mutable: *mutable,
        },
        InferType::Param(n) => InferType::Param(n.clone()),
        InferType::Bool => InferType::Bool,
        InferType::Tuple(elems) => InferType::Tuple(infer_vec_clone(elems)),
        InferType::Enum { path, type_args, lifetime_args } => InferType::Enum {
            path: clone_path(path),
            type_args: infer_vec_clone(type_args),
            lifetime_args: lifetime_repr_vec_clone(lifetime_args),
        },
    }
}

fn infer_vec_clone(v: &Vec<InferType>) -> Vec<InferType> {
    let mut out: Vec<InferType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(infer_clone(&v[i]));
        i += 1;
    }
    out
}

// Build a name → InferType env from a generic struct/template's type-param
// names paired with the call site's type arguments. Used to substitute Param
// in field types or method signatures.
fn build_infer_env(type_params: &Vec<String>, type_args: &Vec<InferType>) -> Vec<(String, InferType)> {
    let mut env: Vec<(String, InferType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), infer_clone(&type_args[i])));
        i += 1;
    }
    env
}

fn rtype_to_infer(rt: &RType) -> InferType {
    match rt {
        RType::Int(k) => InferType::Int(int_kind_copy(k)),
        RType::Struct { path, type_args, lifetime_args } => {
            let mut infer_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                infer_args.push(rtype_to_infer(&type_args[i]));
                i += 1;
            }
            InferType::Struct {
                path: clone_path(path),
                type_args: infer_args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
        RType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
            lifetime: lifetime_repr_clone(lifetime),
        },
        RType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
        },
        RType::Param(n) => InferType::Param(n.clone()),
        RType::Bool => InferType::Bool,
        RType::Tuple(elems) => {
            let mut out: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(rtype_to_infer(&elems[i]));
                i += 1;
            }
            InferType::Tuple(out)
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let mut infer_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                infer_args.push(rtype_to_infer(&type_args[i]));
                i += 1;
            }
            InferType::Enum {
                path: clone_path(path),
                type_args: infer_args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
    }
}

// Substitute type parameters in an InferType using a name → InferType env.
// Used at generic call sites to map the callee's `Param("T")` slots to fresh
// inference vars allocated for the call.
fn infer_substitute(t: &InferType, env: &Vec<(String, InferType)>) -> InferType {
    match t {
        InferType::Var(v) => InferType::Var(*v),
        InferType::Int(k) => InferType::Int(int_kind_copy(k)),
        InferType::Struct { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(infer_substitute(&type_args[i], env));
                i += 1;
            }
            InferType::Struct {
                path: clone_path(path),
                type_args: subst_args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
        InferType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(infer_substitute(inner, env)),
            mutable: *mutable,
            lifetime: lifetime_repr_clone(lifetime),
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_substitute(inner, env)),
            mutable: *mutable,
        },
        InferType::Param(name) => {
            let mut i = 0;
            while i < env.len() {
                if env[i].0 == *name {
                    return infer_clone(&env[i].1);
                }
                i += 1;
            }
            InferType::Param(name.clone())
        }
        InferType::Bool => InferType::Bool,
        InferType::Tuple(elems) => {
            let mut out: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(infer_substitute(&elems[i], env));
                i += 1;
            }
            InferType::Tuple(out)
        }
        InferType::Enum { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(infer_substitute(&type_args[i], env));
                i += 1;
            }
            InferType::Enum {
                path: clone_path(path),
                type_args: subst_args,
                lifetime_args: lifetime_repr_vec_clone(lifetime_args),
            }
        }
    }
}

fn infer_to_string(t: &InferType) -> String {
    match t {
        InferType::Var(v) => format!("?{}", v),
        InferType::Int(k) => int_kind_name(k).to_string(),
        InferType::Struct { path, type_args, .. } => {
            if type_args.is_empty() {
                place_to_string(path)
            } else {
                let mut s = place_to_string(path);
                s.push('<');
                let mut i = 0;
                while i < type_args.len() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&infer_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
        InferType::Ref { inner, mutable, .. } => {
            if *mutable {
                format!("&mut {}", infer_to_string(inner))
            } else {
                format!("&{}", infer_to_string(inner))
            }
        }
        InferType::RawPtr { inner, mutable } => {
            if *mutable {
                format!("*mut {}", infer_to_string(inner))
            } else {
                format!("*const {}", infer_to_string(inner))
            }
        }
        InferType::Param(n) => n.clone(),
        InferType::Bool => "bool".to_string(),
        InferType::Tuple(elems) => {
            let mut s = String::from("(");
            let mut i = 0;
            while i < elems.len() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&infer_to_string(&elems[i]));
                i += 1;
            }
            if elems.len() == 1 {
                s.push(',');
            }
            s.push(')');
            s
        }
        InferType::Enum { path, type_args, .. } => {
            if type_args.is_empty() {
                place_to_string(path)
            } else {
                let mut s = place_to_string(path);
                s.push('<');
                let mut i = 0;
                while i < type_args.len() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&infer_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
    }
}

struct Subst {
    bindings: Vec<Option<InferType>>,
    // Per-var "literal" flag. A var allocated from an integer literal
    // carries this flag; on unification it must resolve to a type that
    // implements `std::Num`. Today that admits every `Int(_)` kind
    // (stdlib provides `impl Num for u8/i8/.../isize`), every user type
    // with `impl Num for ...`, and every `Param(T)` whose bound list
    // includes `Num`. If still unconstrained at body-end, defaults to
    // `i32` (preserving today's literal behavior).
    is_num_lit: Vec<bool>,
}

impl Subst {
    fn fresh_int(&mut self) -> u32 {
        let id = self.bindings.len() as u32;
        self.bindings.push(None);
        self.is_num_lit.push(true);
        id
    }

    fn fresh_var(&mut self) -> u32 {
        let id = self.bindings.len() as u32;
        self.bindings.push(None);
        self.is_num_lit.push(false);
        id
    }

    fn substitute(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Var(v) => match &self.bindings[*v as usize] {
                Some(t) => self.substitute(t),
                None => InferType::Var(*v),
            },
            InferType::Int(k) => InferType::Int(int_kind_copy(k)),
            InferType::Struct { path, type_args, lifetime_args } => {
                let mut subst_args: Vec<InferType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    subst_args.push(self.substitute(&type_args[i]));
                    i += 1;
                }
                InferType::Struct {
                    path: clone_path(path),
                    type_args: subst_args,
                    lifetime_args: lifetime_repr_vec_clone(lifetime_args),
                }
            }
            InferType::Ref { inner, mutable, lifetime } => InferType::Ref {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
                lifetime: lifetime_repr_clone(lifetime),
            },
            InferType::RawPtr { inner, mutable } => InferType::RawPtr {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
            },
            InferType::Param(n) => InferType::Param(n.clone()),
            InferType::Bool => InferType::Bool,
            InferType::Tuple(elems) => {
                let mut out: Vec<InferType> = Vec::new();
                let mut i = 0;
                while i < elems.len() {
                    out.push(self.substitute(&elems[i]));
                    i += 1;
                }
                InferType::Tuple(out)
            }
            InferType::Enum { path, type_args, lifetime_args } => {
                let mut subst_args: Vec<InferType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    subst_args.push(self.substitute(&type_args[i]));
                    i += 1;
                }
                InferType::Enum {
                    path: clone_path(path),
                    type_args: subst_args,
                    lifetime_args: lifetime_repr_vec_clone(lifetime_args),
                }
            }
        }
    }

    fn bind_var(
        &mut self,
        v: u32,
        other: InferType,
        traits: &TraitTable,
        type_params: &Vec<String>,
        type_param_bounds: &Vec<Vec<Vec<String>>>,
        span: &Span,
        file: &str,
    ) -> Result<(), Error> {
        if self.is_num_lit[v as usize] {
            // Var carries the literal-Num bound — verify the candidate
            // satisfies Num. Var-to-Var propagates the flag.
            if let InferType::Var(other_v) = &other {
                self.is_num_lit[*other_v as usize] = true;
            } else if !satisfies_num(&other, traits, type_params, type_param_bounds) {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "type mismatch: expected `{}`, got integer",
                        infer_to_string(&other)
                    ),
                    span: span.copy(),
                });
            }
        }
        self.bindings[v as usize] = Some(other);
        Ok(())
    }


    fn unify(
        &mut self,
        a: &InferType,
        b: &InferType,
        traits: &TraitTable,
        type_params: &Vec<String>,
        type_param_bounds: &Vec<Vec<Vec<String>>>,
        span: &Span,
        file: &str,
    ) -> Result<(), Error> {
        let a = self.substitute(a);
        let b = self.substitute(b);
        match (a, b) {
            (InferType::Var(va), InferType::Var(vb)) => {
                if va == vb {
                    Ok(())
                } else {
                    self.bind_var(
                        va,
                        InferType::Var(vb),
                        traits,
                        type_params,
                        type_param_bounds,
                        span,
                        file,
                    )
                }
            }
            (InferType::Var(v), other) => self.bind_var(
                v,
                other,
                traits,
                type_params,
                type_param_bounds,
                span,
                file,
            ),
            (other, InferType::Var(v)) => self.bind_var(
                v,
                other,
                traits,
                type_params,
                type_param_bounds,
                span,
                file,
            ),
            (InferType::Int(ka), InferType::Int(kb)) => {
                if int_kind_eq(&ka, &kb) {
                    Ok(())
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            int_kind_name(&kb),
                            int_kind_name(&ka)
                        ),
                        span: span.copy(),
                    })
                }
            }
            (
                InferType::Struct {
                    path: pa,
                    type_args: aa,
                    ..
                },
                InferType::Struct {
                    path: pb,
                    type_args: ab,
                    ..
                },
            ) => {
                if !path_eq(&pa, &pb) {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            place_to_string(&pb),
                            place_to_string(&pa)
                        ),
                        span: span.copy(),
                    });
                }
                if aa.len() != ab.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: `{}` has {} type arguments, expected {}",
                            place_to_string(&pa),
                            aa.len(),
                            ab.len()
                        ),
                        span: span.copy(),
                    });
                }
                let mut i = 0;
                while i < aa.len() {
                    self.unify(&aa[i], &ab[i], traits, type_params, type_param_bounds, span, file)?;
                    i += 1;
                }
                Ok(())
            }
            (
                InferType::Ref {
                    inner: ia,
                    mutable: ma,
                    ..
                },
                InferType::Ref {
                    inner: ib,
                    mutable: mb,
                    ..
                },
            ) => {
                if ma != mb {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            if mb {
                                format!("&mut {}", infer_to_string(&ib))
                            } else {
                                format!("&{}", infer_to_string(&ib))
                            },
                            if ma {
                                format!("&mut {}", infer_to_string(&ia))
                            } else {
                                format!("&{}", infer_to_string(&ia))
                            }
                        ),
                        span: span.copy(),
                    });
                }
                self.unify(&ia, &ib, traits, type_params, type_param_bounds, span, file)
            }
            (
                InferType::RawPtr {
                    inner: ia,
                    mutable: ma,
                },
                InferType::RawPtr {
                    inner: ib,
                    mutable: mb,
                },
            ) => {
                if ma != mb {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            if mb {
                                format!("*mut {}", infer_to_string(&ib))
                            } else {
                                format!("*const {}", infer_to_string(&ib))
                            },
                            if ma {
                                format!("*mut {}", infer_to_string(&ia))
                            } else {
                                format!("*const {}", infer_to_string(&ia))
                            }
                        ),
                        span: span.copy(),
                    });
                }
                self.unify(&ia, &ib, traits, type_params, type_param_bounds, span, file)
            }
            (InferType::Param(a), InferType::Param(b)) => {
                if a == b {
                    Ok(())
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            b, a
                        ),
                        span: span.copy(),
                    })
                }
            }
            (InferType::Bool, InferType::Bool) => Ok(()),
            (InferType::Tuple(ea), InferType::Tuple(eb)) => {
                if ea.len() != eb.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "tuple arity mismatch: expected {}-tuple, got {}-tuple",
                            eb.len(),
                            ea.len()
                        ),
                        span: span.copy(),
                    });
                }
                let mut i = 0;
                while i < ea.len() {
                    self.unify(
                        &ea[i],
                        &eb[i],
                        traits,
                        type_params,
                        type_param_bounds,
                        span,
                        file,
                    )?;
                    i += 1;
                }
                Ok(())
            }
            (
                InferType::Enum {
                    path: pa,
                    type_args: aa,
                    ..
                },
                InferType::Enum {
                    path: pb,
                    type_args: ab,
                    ..
                },
            ) => {
                if !path_eq(&pa, &pb) {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            place_to_string(&pb),
                            place_to_string(&pa)
                        ),
                        span: span.copy(),
                    });
                }
                if aa.len() != ab.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: `{}` has {} type arguments, expected {}",
                            place_to_string(&pa),
                            aa.len(),
                            ab.len()
                        ),
                        span: span.copy(),
                    });
                }
                let mut i = 0;
                while i < aa.len() {
                    self.unify(&aa[i], &ab[i], traits, type_params, type_param_bounds, span, file)?;
                    i += 1;
                }
                Ok(())
            }
            (a, b) => Err(Error {
                file: file.to_string(),
                message: format!(
                    "type mismatch: expected `{}`, got `{}`",
                    infer_to_string(&b),
                    infer_to_string(&a)
                ),
                span: span.copy(),
            }),
        }
    }

    fn finalize(&self, ty: &InferType) -> RType {
        match self.substitute(ty) {
            InferType::Var(_) => RType::Int(IntKind::I32),
            InferType::Int(k) => RType::Int(k),
            InferType::Struct { path, type_args, lifetime_args } => {
                let mut concrete: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    concrete.push(self.finalize(&type_args[i]));
                    i += 1;
                }
                RType::Struct {
                    path,
                    type_args: concrete,
                    lifetime_args,
                }
            }
            InferType::Param(n) => RType::Param(n),
            InferType::Ref { inner, mutable, lifetime } => RType::Ref {
                inner: Box::new(self.finalize(&inner)),
                mutable,
                lifetime,
            },
            InferType::RawPtr { inner, mutable } => RType::RawPtr {
                inner: Box::new(self.finalize(&inner)),
                mutable,
            },
            InferType::Bool => RType::Bool,
            InferType::Tuple(elems) => {
                let mut out: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < elems.len() {
                    out.push(self.finalize(&elems[i]));
                    i += 1;
                }
                RType::Tuple(out)
            }
            InferType::Enum { path, type_args, lifetime_args } => {
                let mut concrete: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    concrete.push(self.finalize(&type_args[i]));
                    i += 1;
                }
                RType::Enum {
                    path,
                    type_args: concrete,
                    lifetime_args,
                }
            }
        }
    }
}

struct LitConstraint {
    var: u32,
    value: u64,
    span: Span,
}

struct LocalEntry {
    name: String,
    ty: InferType,
    mutable: bool,
}

struct CheckCtx<'a> {
    locals: Vec<LocalEntry>,
    // Per-NodeId InferType (sized to func.node_count). After body check,
    // each entry is finalized into the FnSymbol/GenericTemplate's expr_types.
    expr_infer_types: Vec<Option<InferType>>,
    lit_constraints: Vec<LitConstraint>,
    // Pending per-MethodCall and per-Call resolutions, indexed by Expr.id.
    method_resolutions: Vec<Option<PendingMethodCall>>,
    call_resolutions: Vec<Option<PendingCall>>,
    subst: Subst,
    current_module: &'a Vec<String>,
    current_file: &'a str,
    structs: &'a StructTable,
    enums: &'a EnumTable,
    traits: &'a TraitTable,
    funcs: &'a FuncTable,
    self_target: Option<&'a RType>,
    type_params: &'a Vec<String>,
    reexports: &'a ReExportTable,
    // Active use entries, ordered with the outermost (module-level)
    // entries first and innermost-block entries appended at the end.
    // Path resolution iterates this in reverse so the innermost scope
    // shadows outer ones. Block walks save `use_scope.len()` before
    // entering and truncate back on exit.
    use_scope: Vec<UseEntry>,
    // Per-type-param trait bounds (resolved trait paths) for the
    // currently-checked function. Same shape as
    // `GenericTemplate.type_param_bounds` — `[i]` lists the bound traits
    // on `type_params[i]`. Empty for non-generic functions.
    type_param_bounds: &'a Vec<Vec<Vec<String>>>,
}

fn check_module(
    module: &Module,
    path: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let crate_root: &str = if path.is_empty() { "" } else { &path[0] };
    let use_scope = module_use_entries(module, crate_root);
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                check_function(f, path, path, None, current_file, structs, enums, traits, funcs, reexports, &use_scope)?
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                check_module(m, path, current_file, structs, enums, traits, funcs, reexports)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(ib) => {
                let target_rt = resolve_impl_target(ib, path, structs, enums, &use_scope, reexports, current_file)?;
                // T2.6: mirror the prefix collect_funcs used. For Path
                // targets, that's the first AST segment. For non-Path
                // trait impls, it's a synthetic `__trait_impl_<idx>`
                // matching the registration order of `traits.impls`.
                let mut method_prefix = clone_path(path);
                match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if !p.segments.is_empty() => {
                        method_prefix.push(p.segments[0].name.clone());
                    }
                    _ => {
                        match find_trait_impl_idx(ib, &target_rt, path, traits, &use_scope, reexports, current_file) {
                            Some(idx) => {
                                method_prefix.push(format!("__trait_impl_{}", idx));
                            }
                            None => {
                                // Inherent impl with non-path target —
                                // already rejected in collect_funcs.
                                i += 1;
                                continue;
                            }
                        }
                    }
                }
                let mut k = 0;
                while k < ib.methods.len() {
                    check_function(
                        &ib.methods[k],
                        path,
                        &method_prefix,
                        Some(&target_rt),
                        current_file,
                        structs,
                        enums,
                        traits,
                        funcs,
                        reexports,
                        &use_scope,
                    )?;
                    k += 1;
                }
            }
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
    *current_file = saved;
    Ok(())
}

fn check_function(
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    current_file: &str,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
    reexports: &ReExportTable,
    module_use_scope: &Vec<UseEntry>,
) -> Result<(), Error> {
    // Look up the registered template to derive the full type-param list
    // (impl's params + method's own params, for generic impl methods).
    let lookup_path = {
        let mut p = clone_path(path_prefix);
        p.push(func.name.clone());
        p
    };
    let (type_param_names, type_param_bounds): (Vec<String>, Vec<Vec<Vec<String>>>) =
        match template_lookup(funcs, &lookup_path) {
            Some((_, t)) => {
                let mut bounds_clone: Vec<Vec<Vec<String>>> = Vec::new();
                let mut i = 0;
                while i < t.type_param_bounds.len() {
                    let mut row: Vec<Vec<String>> = Vec::new();
                    let mut j = 0;
                    while j < t.type_param_bounds[i].len() {
                        row.push(t.type_param_bounds[i][j].clone());
                        j += 1;
                    }
                    bounds_clone.push(row);
                    i += 1;
                }
                (t.type_params.clone(), bounds_clone)
            }
            None => (Vec::new(), Vec::new()),
        };
    // Build initial locals from params (params are immutable bindings in our subset).
    let mut locals: Vec<LocalEntry> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let rt = resolve_type(
            &func.params[k].ty,
            current_module,
            structs,
            enums,
            self_target,
            &type_param_names,
            module_use_scope,
            reexports,
            current_file,
        )?;
        locals.push(LocalEntry {
            name: func.params[k].name.clone(),
            ty: rtype_to_infer(&rt),
            mutable: false,
        });
        k += 1;
    }
    let return_rt: Option<RType> = match &func.return_type {
        Some(ty) => Some(resolve_type(
            ty,
            current_module,
            structs,
            enums,
            self_target,
            &type_param_names,
            module_use_scope,
            reexports,
            current_file,
        )?),
        None => None,
    };

    let node_count = func.node_count as usize;
    let (expr_infer_types, lit_constraints, method_resolutions, call_resolutions, subst) = {
        let mut method_res: Vec<Option<PendingMethodCall>> = Vec::with_capacity(node_count);
        let mut call_res: Vec<Option<PendingCall>> = Vec::with_capacity(node_count);
        let mut expr_infer: Vec<Option<InferType>> = Vec::with_capacity(node_count);
        let mut i = 0;
        while i < node_count {
            method_res.push(None);
            call_res.push(None);
            expr_infer.push(None);
            i += 1;
        }
        // Initialize the use scope with the module's flattened entries.
        // Inner blocks push their own `Stmt::Use` entries on top during
        // body traversal; the scope is restored on block exit.
        let mut initial_use_scope: Vec<UseEntry> = Vec::new();
        let mut k = 0;
        while k < module_use_scope.len() {
            initial_use_scope.push(module_use_scope[k].clone());
            k += 1;
        }
        let mut ctx = CheckCtx {
            locals,
            expr_infer_types: expr_infer,
            lit_constraints: Vec::new(),
            method_resolutions: method_res,
            call_resolutions: call_res,
            subst: Subst {
                bindings: Vec::new(),
                is_num_lit: Vec::new(),
            },
            current_module,
            current_file,
            structs,
            enums,
            traits,
            funcs: &*funcs,
            self_target,
            type_params: &type_param_names,
            type_param_bounds: &type_param_bounds,
            reexports,
            use_scope: initial_use_scope,
        };
        check_block(&mut ctx, &func.body, &return_rt)?;
        (
            ctx.expr_infer_types,
            ctx.lit_constraints,
            ctx.method_resolutions,
            ctx.call_resolutions,
            ctx.subst,
        )
    };

    // Range-check each integer literal against its (now resolved) type.
    // T5.5: a literal may resolve to a non-`Int` type (a user `impl Num
    // for Foo`, or a generic `Param(T)` with `T: Num`); range-checking
    // doesn't apply there — the user's `from_i64` decides the i64 →
    // user-type semantics.
    let mut i = 0;
    while i < lit_constraints.len() {
        let lc = &lit_constraints[i];
        let resolved = subst.substitute(&InferType::Var(lc.var));
        let kind = match resolved {
            InferType::Var(_) => IntKind::I32,
            InferType::Int(k) => k,
            _ => {
                // Non-Int target (Struct/Param/etc.) — skip range check.
                i += 1;
                continue;
            }
        };
        if (lc.value as u128) > int_kind_max(&kind) {
            return Err(Error {
                file: current_file.to_string(),
                message: format!(
                    "integer literal `{}` does not fit in `{}`",
                    lc.value,
                    int_kind_name(&kind)
                ),
                span: lc.span.copy(),
            });
        }
        i += 1;
    }

    // Finalize per-NodeId expr types.
    let mut expr_types: Vec<Option<RType>> = Vec::with_capacity(node_count);
    let mut i = 0;
    while i < expr_infer_types.len() {
        match &expr_infer_types[i] {
            Some(t) => expr_types.push(Some(subst.finalize(t))),
            None => expr_types.push(None),
        }
        i += 1;
    }
    // Finalize method resolutions (per-NodeId).
    let mut method_resolutions_final: Vec<Option<MethodResolution>> =
        Vec::with_capacity(node_count);
    let mut i = 0;
    while i < method_resolutions.len() {
        match &method_resolutions[i] {
            Some(p) => {
                let mut type_args: Vec<RType> = Vec::new();
                let mut j = 0;
                while j < p.type_arg_infers.len() {
                    type_args.push(subst.finalize(&p.type_arg_infers[j]));
                    j += 1;
                }
                let trait_dispatch = match &p.trait_dispatch {
                    Some(td) => Some(TraitDispatch {
                        trait_path: clone_path(&td.trait_path),
                        method_name: td.method_name.clone(),
                        recv_type: subst.finalize(&td.recv_type_infer),
                    }),
                    None => None,
                };
                method_resolutions_final.push(Some(MethodResolution {
                    callee_idx: p.callee_idx,
                    callee_path: clone_path(&p.callee_path),
                    recv_adjust: copy_recv_adjust_local(&p.recv_adjust),
                    ret_borrows_receiver: p.ret_borrows_receiver,
                    template_idx: p.template_idx,
                    type_args,
                    trait_dispatch,
                }));
            }
            None => method_resolutions_final.push(None),
        }
        i += 1;
    }
    let method_resolutions = method_resolutions_final;
    // Finalize call resolutions (per-NodeId).
    let mut call_resolutions_final: Vec<Option<CallResolution>> =
        Vec::with_capacity(node_count);
    let mut i = 0;
    while i < call_resolutions.len() {
        match &call_resolutions[i] {
            Some(PendingCall::Direct(idx)) => {
                call_resolutions_final.push(Some(CallResolution::Direct(*idx)))
            }
            Some(PendingCall::Generic { template_idx, type_var_ids }) => {
                let mut concrete: Vec<RType> = Vec::new();
                let mut j = 0;
                while j < type_var_ids.len() {
                    concrete.push(subst.finalize(&InferType::Var(type_var_ids[j])));
                    j += 1;
                }
                call_resolutions_final.push(Some(CallResolution::Generic {
                    template_idx: *template_idx,
                    type_args: concrete,
                }));
            }
            Some(PendingCall::Variant { enum_path, disc, type_var_ids }) => {
                let mut concrete: Vec<RType> = Vec::new();
                let mut j = 0;
                while j < type_var_ids.len() {
                    concrete.push(subst.finalize(&InferType::Var(type_var_ids[j])));
                    j += 1;
                }
                call_resolutions_final.push(Some(CallResolution::Variant {
                    enum_path: clone_path(enum_path),
                    disc: *disc,
                    type_args: concrete,
                }));
            }
            None => call_resolutions_final.push(None),
        }
        i += 1;
    }
    let call_resolutions = call_resolutions_final;

    // Store on the FnSymbol or GenericTemplate.
    let mut full = clone_path(path_prefix);
    full.push(func.name.clone());
    let mut entry_idx: Option<usize> = None;
    let mut e = 0;
    while e < funcs.entries.len() {
        if path_eq(&funcs.entries[e].path, &full) {
            entry_idx = Some(e);
            break;
        }
        e += 1;
    }
    if let Some(e) = entry_idx {
        funcs.entries[e].expr_types = expr_types;
        funcs.entries[e].method_resolutions = method_resolutions;
        funcs.entries[e].call_resolutions = call_resolutions;
    } else {
        let mut t = 0;
        while t < funcs.templates.len() {
            if path_eq(&funcs.templates[t].path, &full) {
                funcs.templates[t].expr_types = expr_types;
                funcs.templates[t].method_resolutions = method_resolutions;
                funcs.templates[t].call_resolutions = call_resolutions;
                break;
            }
            t += 1;
        }
    }
    Ok(())
}

fn copy_recv_adjust_local(r: &ReceiverAdjust) -> ReceiverAdjust {
    match r {
        ReceiverAdjust::Move => ReceiverAdjust::Move,
        ReceiverAdjust::BorrowImm => ReceiverAdjust::BorrowImm,
        ReceiverAdjust::BorrowMut => ReceiverAdjust::BorrowMut,
        ReceiverAdjust::ByRef => ReceiverAdjust::ByRef,
    }
}

// Per-call recording during body check; resolved at end-of-fn into `CallResolution`.
enum PendingCall {
    Direct(usize),
    Generic { template_idx: usize, type_var_ids: Vec<u32> },
    Variant {
        enum_path: Vec<String>,
        disc: u32,
        // One InferType per enum's type-param, allocated as a fresh
        // var per construction site (or set to a concrete value if
        // the user wrote turbofish). Finalized at end-of-fn.
        type_var_ids: Vec<u32>,
    },
}

// Like `MethodResolution`, but records type-arg InferTypes instead of
// finalized RTypes. End-of-fn finalizes via `subst.finalize`.
struct PendingMethodCall {
    callee_idx: u32,
    callee_path: Vec<String>,
    recv_adjust: ReceiverAdjust,
    ret_borrows_receiver: bool,
    template_idx: Option<usize>,
    // For template methods: one InferType per template type_param. Order:
    // impl's params first (bound to receiver type_args), then method's own
    // params (fresh inference vars, possibly unified by turbofish/inference).
    type_arg_infers: Vec<InferType>,
    // T2: when the call is dispatched symbolically through a trait
    // bound (recv is `Param(T)` with `T: Trait`), record the trait path,
    // method name, and the receiver's InferType so codegen can re-resolve
    // the impl after monomorphization. None for direct dispatch.
    trait_dispatch: Option<PendingTraitDispatch>,
}

struct PendingTraitDispatch {
    trait_path: Vec<String>,
    method_name: String,
    recv_type_infer: InferType,
}

fn check_block(
    ctx: &mut CheckCtx,
    block: &Block,
    return_type: &Option<RType>,
) -> Result<(), Error> {
    let actual = check_block_inner(ctx, block)?;
    // No declared return type ⇒ function returns `()` (the unit tuple).
    let expected: RType = match return_type {
        Some(rt) => rtype_clone(rt),
        None => RType::Tuple(Vec::new()),
    };
    let expected_infer = rtype_to_infer(&expected);
    ctx.subst.unify(
        &actual,
        &expected_infer,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &tail_span_or_block(block),
        ctx.current_file,
    )?;
    Ok(())
}

// A block always has a type. With a tail expression, it's the tail's
// type; without one, it's `()` (the empty tuple).
fn check_block_inner(ctx: &mut CheckCtx, block: &Block) -> Result<InferType, Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => check_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => check_assign_stmt(ctx, assign)?,
            Stmt::Expr(expr) => check_expr_stmt(ctx, expr)?,
            Stmt::Use(decl) => {
                let crate_root: &str = if ctx.current_module.is_empty() {
                    ""
                } else {
                    &ctx.current_module[0]
                };
                flatten_use_tree(&Vec::new(), &decl.tree, crate_root, decl.is_pub, &mut ctx.use_scope);
            }
        }
        i += 1;
    }
    match &block.tail {
        Some(expr) => Ok(check_expr(ctx, expr)?),
        None => Ok(InferType::Tuple(Vec::new())),
    }
}

// `expr;` — type-check the expression, then discard its value (any
// type is fine in stmt position). Any expression may sit here now that
// we have a unit type.
fn check_expr_stmt(ctx: &mut CheckCtx, expr: &Expr) -> Result<(), Error> {
    let _ = check_expr(ctx, expr)?;
    Ok(())
}

fn check_block_expr(ctx: &mut CheckCtx, block: &Block) -> Result<InferType, Error> {
    let mark = ctx.locals.len();
    let use_mark = ctx.use_scope.len();
    let ty = check_block_inner(ctx, block)?;
    ctx.locals.truncate(mark);
    ctx.use_scope.truncate(use_mark);
    Ok(ty)
}

fn tail_span_or_block(block: &Block) -> Span {
    match &block.tail {
        Some(expr) => expr.span.copy(),
        None => block.span.copy(),
    }
}

fn check_let_stmt(ctx: &mut CheckCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    let value_ty = check_expr(ctx, &let_stmt.value)?;
    let final_ty = match &let_stmt.ty {
        Some(annotation) => {
            let annot_rt = resolve_type(
                annotation,
                ctx.current_module,
                ctx.structs,
                ctx.enums,
                ctx.self_target,
                ctx.type_params,
                &ctx.use_scope,
                ctx.reexports,
                ctx.current_file,
            )?;
            let annot_infer = rtype_to_infer(&annot_rt);
            ctx.subst.unify(
                &value_ty,
                &annot_infer,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &let_stmt.value.span,
                ctx.current_file,
            )?;
            annot_infer
        }
        None => value_ty,
    };
    // Overwrite the recorded type at the value expr's id with the final type
    // (in case an annotation pinned it down). Codegen reads expr_types[value.id]
    // to size the binding's storage.
    ctx.expr_infer_types[let_stmt.value.id as usize] = Some(infer_clone(&final_ty));
    ctx.locals.push(LocalEntry {
        name: let_stmt.name.clone(),
        ty: final_ty,
        mutable: let_stmt.mutable,
    });
    Ok(())
}

fn check_assign_stmt(ctx: &mut CheckCtx, assign: &AssignStmt) -> Result<(), Error> {
    // Two flavors of LHS:
    //   1. Var-rooted chain: `x` or `x.f.g.h`.
    //   2. Deref-rooted chain: `*p` or `(*p).f.g.h`.
    if let Some((root_expr, fields)) = extract_deref_rooted_chain(&assign.lhs) {
        return check_deref_rooted_assign(ctx, root_expr, &fields, assign);
    }
    // LHS must be a place expression (Var or Var-rooted FieldAccess chain).
    let chain = match extract_place_for_assign(&assign.lhs) {
        Some(c) => c,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "left-hand side of assignment must be a place expression".to_string(),
                span: assign.lhs.span.copy(),
            });
        }
    };
    // Find root binding (search reverse for innermost shadow).
    let mut found_idx: Option<usize> = None;
    let mut i = ctx.locals.len();
    while i > 0 {
        i -= 1;
        if ctx.locals[i].name == chain[0] {
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown variable: `{}`", chain[0]),
                span: assign.lhs.span.copy(),
            });
        }
    };
    let root_resolved = ctx.subst.substitute(&ctx.locals[idx].ty);
    let root_is_mut_ref = matches!(root_resolved, InferType::Ref { mutable: true, .. });
    let root_is_shared_ref = matches!(root_resolved, InferType::Ref { mutable: false, .. });
    if chain.len() == 1 {
        if !ctx.locals[idx].mutable {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot assign to `{}`: not declared as `mut`",
                    chain[0]
                ),
                span: assign.lhs.span.copy(),
            });
        }
    } else if root_is_shared_ref {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot assign through `{}`: shared reference is not mutable",
                chain[0]
            ),
            span: assign.lhs.span.copy(),
        });
    } else if !root_is_mut_ref && !ctx.locals[idx].mutable {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot assign to field of `{}`: not declared as `mut`",
                chain[0]
            ),
            span: assign.lhs.span.copy(),
        });
    }
    // Walk the chain to determine the LHS type.
    let lhs_ty = walk_chain_type(
        &ctx.locals[idx].ty,
        &chain,
        ctx.structs,
        ctx.enums,
        ctx.current_file,
        &assign.lhs.span,
    )?;
    let rhs_ty = check_expr(ctx, &assign.rhs)?;
    ctx.subst.unify(
        &rhs_ty,
        &lhs_ty,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &assign.rhs.span,
        ctx.current_file,
    )?;
    Ok(())
}

// Returns (deref_target, field_chain) if the LHS is `*expr` or
// `(*expr).field…`. The deref_target is the expression being dereferenced
// (typically a Var bound to a `&mut T` / `*mut T`); the field_chain is the
// list of fields walked after the deref.
fn extract_deref_rooted_chain<'a>(expr: &'a Expr) -> Option<(&'a Expr, Vec<String>)> {
    let mut fields: Vec<String> = Vec::new();
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Deref(inner) => {
                let mut reversed: Vec<String> = Vec::new();
                let mut i = fields.len();
                while i > 0 {
                    i -= 1;
                    reversed.push(fields[i].clone());
                }
                return Some((inner.as_ref(), reversed));
            }
            ExprKind::FieldAccess(fa) => {
                fields.push(fa.field.clone());
                current = &fa.base;
            }
            _ => return None,
        }
    }
}

fn check_deref_rooted_assign(
    ctx: &mut CheckCtx,
    root_expr: &Expr,
    fields: &Vec<String>,
    assign: &AssignStmt,
) -> Result<(), Error> {
    // The root must type as `&mut T` or `*mut T` — otherwise the deref isn't
    // assignable. (We don't allow whole-place assignment through `*const T`
    // or `&T`, matching Rust.)
    let root_infer = check_expr(ctx, root_expr)?;
    let resolved = ctx.subst.substitute(&root_infer);
    let inner_infer = match resolved {
        InferType::Ref { inner, mutable: true, .. } => *inner,
        InferType::RawPtr { inner, mutable: true } => *inner,
        InferType::Ref { mutable: false, .. } => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "cannot assign through a shared reference".to_string(),
                span: assign.lhs.span.copy(),
            });
        }
        InferType::RawPtr { mutable: false, .. } => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "cannot assign through a `*const T` raw pointer".to_string(),
                span: assign.lhs.span.copy(),
            });
        }
        other => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot dereference `{}` for assignment",
                    infer_to_string(&other)
                ),
                span: assign.lhs.span.copy(),
            });
        }
    };
    // Walk fields starting from the pointed-at type to find the LHS type.
    let mut current = inner_infer;
    let mut i = 0;
    while i < fields.len() {
        let (struct_path, type_args) = match &current {
            InferType::Struct { path, type_args, .. } => (clone_path(path), infer_vec_clone(type_args)),
            _ => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "field assignment on non-struct value".to_string(),
                    span: assign.lhs.span.copy(),
                });
            }
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let mut found = false;
        let mut k = 0;
        while k < entry.fields.len() {
            if entry.fields[k].name == fields[i] {
                let field_infer = rtype_to_infer(&entry.fields[k].ty);
                let env = build_infer_env(&entry.type_params, &type_args);
                current = infer_substitute(&field_infer, &env);
                found = true;
                break;
            }
            k += 1;
        }
        if !found {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "no field `{}` on `{}`",
                    fields[i],
                    place_to_string(&struct_path)
                ),
                span: assign.lhs.span.copy(),
            });
        }
        i += 1;
    }
    let rhs_ty = check_expr(ctx, &assign.rhs)?;
    ctx.subst.unify(
        &rhs_ty,
        &current,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &assign.rhs.span,
        ctx.current_file,
    )?;
    Ok(())
}

fn extract_place_for_assign(expr: &Expr) -> Option<Vec<String>> {
    let mut chain: Vec<String> = Vec::new();
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Var(name) => {
                chain.push(name.clone());
                let mut reversed: Vec<String> = Vec::new();
                let mut i = chain.len();
                while i > 0 {
                    i -= 1;
                    reversed.push(chain[i].clone());
                }
                return Some(reversed);
            }
            ExprKind::FieldAccess(fa) => {
                chain.push(fa.field.clone());
                current = &fa.base;
            }
            ExprKind::TupleIndex { base, index, .. } => {
                chain.push(format!("{}", index));
                current = base;
            }
            _ => return None,
        }
    }
}

fn walk_chain_type(
    start: &InferType,
    chain: &Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    file: &str,
    span: &Span,
) -> Result<InferType, Error> {
    let mut current = infer_clone(start);
    let mut i = 1;
    while i < chain.len() {
        // Tuple-index chain segment: digit-only string. The type
        // of the segment is the corresponding tuple element. This
        // takes precedence over struct-field lookup so a struct
        // with a `0` field would still resolve there only if the
        // current type is actually a struct.
        let is_tuple_seg = !chain[i].is_empty() && chain[i].bytes().all(|b| b.is_ascii_digit());
        if is_tuple_seg {
            let elems: Vec<InferType> = match &current {
                InferType::Tuple(es) => infer_vec_clone(es),
                InferType::Ref { inner, .. } => match inner.as_ref() {
                    InferType::Tuple(es) => infer_vec_clone(es),
                    _ => {
                        return Err(Error {
                            file: file.to_string(),
                            message: "tuple-index assignment on non-tuple value".to_string(),
                            span: span.copy(),
                        });
                    }
                },
                _ => {
                    return Err(Error {
                        file: file.to_string(),
                        message: "tuple-index assignment on non-tuple value".to_string(),
                        span: span.copy(),
                    });
                }
            };
            let idx: usize = chain[i].parse().expect("digit-only segment");
            if idx >= elems.len() {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "tuple index {} out of range (length {})",
                        idx,
                        elems.len()
                    ),
                    span: span.copy(),
                });
            }
            current = infer_clone(&elems[idx]);
            i += 1;
            continue;
        }
        let (struct_path, type_args) = match &current {
            InferType::Struct { path, type_args, .. } => (clone_path(path), infer_vec_clone(type_args)),
            InferType::Ref { inner, .. } => match inner.as_ref() {
                InferType::Struct { path, type_args, .. } => {
                    (clone_path(path), infer_vec_clone(type_args))
                }
                _ => {
                    return Err(Error {
                        file: file.to_string(),
                        message: "field assignment on non-struct value".to_string(),
                        span: span.copy(),
                    });
                }
            },
            _ => {
                return Err(Error {
                    file: file.to_string(),
                    message: "field assignment on non-struct value".to_string(),
                    span: span.copy(),
                });
            }
        };
        let entry = struct_lookup(structs, &struct_path).expect("resolved struct");
        let mut found = false;
        let mut k = 0;
        while k < entry.fields.len() {
            if entry.fields[k].name == chain[i] {
                let field_infer = rtype_to_infer(&entry.fields[k].ty);
                let env = build_infer_env(&entry.type_params, &type_args);
                current = infer_substitute(&field_infer, &env);
                found = true;
                break;
            }
            k += 1;
        }
        if !found {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "no field `{}` on `{}`",
                    chain[i],
                    place_to_string(&struct_path)
                ),
                span: span.copy(),
            });
        }
        i += 1;
    }
    Ok(current)
}

fn check_expr(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
    let ty = check_expr_inner(ctx, expr)?;
    // Record the resolved InferType under this Expr's NodeId. Finalized to
    // RType at end-of-fn into FnSymbol/Template.expr_types.
    ctx.expr_infer_types[expr.id as usize] = Some(infer_clone(&ty));
    Ok(ty)
}

fn check_expr_inner(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
    match &expr.kind {
        ExprKind::IntLit(n) => {
            let v = ctx.subst.fresh_int();
            ctx.lit_constraints.push(LitConstraint {
                var: v,
                value: *n,
                span: expr.span.copy(),
            });
            Ok(InferType::Var(v))
        }
        ExprKind::Var(name) => {
            let mut i = ctx.locals.len();
            while i > 0 {
                i -= 1;
                if ctx.locals[i].name == *name {
                    return Ok(infer_clone(&ctx.locals[i].ty));
                }
            }
            Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown variable: `{}`", name),
                span: expr.span.copy(),
            })
        }
        ExprKind::Call(call) => check_call(ctx, call, expr),
        ExprKind::StructLit(lit) => check_struct_lit(ctx, lit, expr),
        ExprKind::FieldAccess(fa) => check_field_access(ctx, fa, expr),
        ExprKind::Borrow { inner, mutable } => {
            // Walk the inner as a place expression — borrowing a non-Copy
            // place doesn't move out, so the "Copy through ref" check that
            // applies to value-position field access doesn't fire here.
            let inner_ty = check_place_expr(ctx, inner)?;
            // Phase B: borrow expressions get an `Inferred(0)` placeholder
            // lifetime; refining this into per-borrow fresh lifetimes is
            // Phase C's job.
            Ok(InferType::Ref {
                inner: Box::new(inner_ty),
                mutable: *mutable,
                lifetime: LifetimeRepr::Inferred(0),
            })
        }
        ExprKind::Cast { inner, ty } => check_cast(ctx, inner, ty, expr),
        ExprKind::Deref(inner) => check_deref(ctx, inner, expr),
        ExprKind::Unsafe(block) => check_block_expr(ctx, block.as_ref()),
        ExprKind::Block(block) => check_block_expr(ctx, block.as_ref()),
        ExprKind::MethodCall(mc) => check_method_call(ctx, mc, expr),
        ExprKind::BoolLit(_) => Ok(InferType::Bool),
        ExprKind::If(if_expr) => check_if_expr(ctx, if_expr, expr),
        ExprKind::Builtin { name, name_span, args } => {
            check_builtin(ctx, name, name_span, args, expr)
        }
        ExprKind::Tuple(elems) => {
            let mut tys: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                tys.push(check_expr(ctx, &elems[i])?);
                i += 1;
            }
            Ok(InferType::Tuple(tys))
        }
        ExprKind::TupleIndex { base, index, index_span } => {
            let base_ty = check_expr(ctx, base)?;
            // Auto-deref through any number of references — `r.0` on
            // `&(u32, u32)` reads element 0. (Tuple elements that are
            // non-Copy through a ref will still be rejected by the
            // borrow-aware path, but for now we only have integer/bool
            // tuples in tests.)
            let mut cur = ctx.subst.substitute(&base_ty);
            while let InferType::Ref { inner, .. } = cur {
                cur = ctx.subst.substitute(&inner);
            }
            match cur {
                InferType::Tuple(elems) => {
                    let n = elems.len();
                    if (*index as usize) >= n {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message: format!(
                                "tuple index {} out of range (length {})",
                                index, n
                            ),
                            span: index_span.copy(),
                        });
                    }
                    Ok(infer_clone(&elems[*index as usize]))
                }
                other => Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "tuple index `.{}` on non-tuple type `{}`",
                        index,
                        infer_to_string(&other)
                    ),
                    span: expr.span.copy(),
                }),
            }
        }
        ExprKind::Match(m) => check_match_expr(ctx, m, expr),
        ExprKind::IfLet(il) => check_if_let_expr(ctx, il, expr),
    }
}

// `if cond { … } else { … }` — cond must be `bool`, the two arms'
// tail types unify, and the if-expression takes that type. A
// tail-less arm yields `()`, so a both-tail-less if is unit-typed.
fn check_if_expr(
    ctx: &mut CheckCtx,
    if_expr: &crate::ast::IfExpr,
    outer: &Expr,
) -> Result<InferType, Error> {
    // `check_expr` (not `check_expr_inner`) so the cond's type is
    // recorded under its NodeId — codegen for some sub-exprs (e.g.
    // `Builtin`) reads its own result type back out.
    let cond_ty = check_expr(ctx, &if_expr.cond)?;
    ctx.subst.unify(
        &cond_ty,
        &InferType::Bool,
        ctx.traits,
        &ctx.type_params,
        &ctx.type_param_bounds,
        &if_expr.cond.span,
        ctx.current_file,
    )?;
    let then_ty = check_block_expr(ctx, if_expr.then_block.as_ref())?;
    let else_ty = check_block_expr(ctx, if_expr.else_block.as_ref())?;
    ctx.subst.unify(
        &then_ty,
        &else_ty,
        ctx.traits,
        &ctx.type_params,
        &ctx.type_param_bounds,
        &outer.span,
        ctx.current_file,
    )?;
    Ok(then_ty)
}

// `match scrut { pat1 => arm1, pat2 if guard => arm2, _ => arm3 }`.
// All arms unify to the same type. Patterns introduce bindings that
// scope to the arm's body. Guards are not yet supported. Exhaustiveness
// is checked structurally per scrutinee type.
fn check_match_expr(
    ctx: &mut CheckCtx,
    m: &crate::ast::MatchExpr,
    outer: &Expr,
) -> Result<InferType, Error> {
    let scrutinee_ty = check_expr(ctx, &m.scrutinee)?;
    if m.arms.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "match expression must have at least one arm".to_string(),
            span: m.span.copy(),
        });
    }
    let mut arm_ty: Option<InferType> = None;
    let mut i = 0;
    let mut any_guard = false;
    while i < m.arms.len() {
        let arm = &m.arms[i];
        if arm.guard.is_some() {
            any_guard = true;
        }
        // Type-check the pattern against the scrutinee type and collect
        // the bindings it introduces. Push them as locals for the arm
        // body (and guard, if any), then truncate when the arm is done.
        let mark = ctx.locals.len();
        let mut bindings: Vec<(String, InferType, Span, bool)> = Vec::new();
        check_pattern(ctx, &arm.pattern, &scrutinee_ty, &mut bindings)?;
        let mut k = 0;
        while k < bindings.len() {
            ctx.locals.push(LocalEntry {
                name: bindings[k].0.clone(),
                ty: infer_clone(&bindings[k].1),
                mutable: bindings[k].3,
            });
            k += 1;
        }
        // Guard: a `bool`-typed expression that runs after the pattern
        // matches but before the body. Bindings are in scope.
        if let Some(g) = &arm.guard {
            let g_ty = check_expr(ctx, g)?;
            ctx.subst.unify(
                &g_ty,
                &InferType::Bool,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &g.span,
                ctx.current_file,
            )?;
        }
        let body_ty = check_expr(ctx, &arm.body)?;
        ctx.locals.truncate(mark);
        match &arm_ty {
            Some(prev) => {
                let prev_clone = infer_clone(prev);
                ctx.subst.unify(
                    &body_ty,
                    &prev_clone,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    &arm.body.span,
                    ctx.current_file,
                )?;
            }
            None => arm_ty = Some(body_ty),
        }
        i += 1;
    }
    // Exhaustiveness: every value of the scrutinee's type must be
    // matched by at least one arm. Substitute the scrutinee type so
    // we have its concrete shape.
    let scrutinee_concrete = ctx.subst.substitute(&scrutinee_ty);
    check_match_exhaustive(
        ctx,
        &scrutinee_concrete,
        &m.arms,
        &outer.span,
    )?;
    Ok(arm_ty.expect("match has at least one arm"))
}

// `if let Pat = scrut { then } else { else }`. Like a single-arm
// match plus an `else` fallback, with the pattern bindings scoped to
// the then-block. `else` is optional in source — the parser already
// substitutes an empty unit-typed block when no `else` was written.
// Both arms unify to the same type, like a regular `if`. No
// exhaustiveness check (the `else` covers non-matches).
fn check_if_let_expr(
    ctx: &mut CheckCtx,
    il: &crate::ast::IfLetExpr,
    outer: &Expr,
) -> Result<InferType, Error> {
    let scrutinee_ty = check_expr(ctx, &il.scrutinee)?;
    let mark = ctx.locals.len();
    let mut bindings: Vec<(String, InferType, Span, bool)> = Vec::new();
    check_pattern(ctx, &il.pattern, &scrutinee_ty, &mut bindings)?;
    let mut k = 0;
    while k < bindings.len() {
        ctx.locals.push(LocalEntry {
            name: bindings[k].0.clone(),
            ty: infer_clone(&bindings[k].1),
            mutable: bindings[k].3,
        });
        k += 1;
    }
    let then_ty = check_block_expr(ctx, il.then_block.as_ref())?;
    ctx.locals.truncate(mark);
    let else_ty = check_block_expr(ctx, il.else_block.as_ref())?;
    ctx.subst.unify(
        &then_ty,
        &else_ty,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &outer.span,
        ctx.current_file,
    )?;
    Ok(then_ty)
}

// Type-check a pattern against `scrutinee_ty`, appending `(name, ty,
// span)` for every binding the pattern introduces. Recurses into
// sub-patterns. The final pattern type is `scrutinee_ty` itself
// (patterns are checked for compatibility, not unified to a different
// type).
fn check_pattern(
    ctx: &mut CheckCtx,
    pattern: &Pattern,
    scrutinee_ty: &InferType,
    bindings: &mut Vec<(String, InferType, Span, bool)>,
) -> Result<(), Error> {
    use crate::ast::PatternKind;
    // Record the resolved scrutinee type at this pattern's NodeId so
    // codegen can look it up directly without re-inferring.
    ctx.expr_infer_types[pattern.id as usize] = Some(infer_clone(scrutinee_ty));
    match &pattern.kind {
        PatternKind::Wildcard => Ok(()),
        PatternKind::LitInt(_) => {
            let resolved = ctx.subst.substitute(scrutinee_ty);
            match resolved {
                InferType::Int(_) | InferType::Var(_) => {
                    // Pin the var to int via unification with a fresh int-class var.
                    let v = ctx.subst.fresh_int();
                    ctx.subst.unify(
                        scrutinee_ty,
                        &InferType::Var(v),
                        ctx.traits,
                        ctx.type_params,
                        ctx.type_param_bounds,
                        &pattern.span,
                        ctx.current_file,
                    )?;
                    Ok(())
                }
                other => Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "integer literal pattern but scrutinee has type `{}`",
                        infer_to_string(&other)
                    ),
                    span: pattern.span.copy(),
                }),
            }
        }
        PatternKind::LitBool(_) => {
            ctx.subst.unify(
                scrutinee_ty,
                &InferType::Bool,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &pattern.span,
                ctx.current_file,
            )
        }
        PatternKind::Binding { name, by_ref, mutable, .. } => {
            // `name` / `mut name`: bind the matched value by-value;
            // the binding's type is the scrutinee's type. The
            // `mutable` flag flows into the local entry so writes
            // through the binding are allowed only for `mut name`.
            // `ref name` / `ref mut name`: bind a reference to the
            // matched place; the binding's type is `&T` / `&mut T`.
            // The binding itself is non-mut (the ref's mutability is
            // baked into its type).
            let binding_ty = if *by_ref {
                InferType::Ref {
                    inner: Box::new(infer_clone(scrutinee_ty)),
                    mutable: *mutable,
                    lifetime: LifetimeRepr::Inferred(0),
                }
            } else {
                infer_clone(scrutinee_ty)
            };
            let bind_mutable = !*by_ref && *mutable;
            bindings.push((name.clone(), binding_ty, pattern.span.copy(), bind_mutable));
            Ok(())
        }
        PatternKind::VariantTuple { path, elems } => {
            check_variant_tuple_pattern(ctx, path, elems, scrutinee_ty, &pattern.span, bindings)
        }
        PatternKind::VariantStruct { path, fields, rest } => {
            check_variant_struct_pattern(
                ctx,
                path,
                fields,
                *rest,
                scrutinee_ty,
                &pattern.span,
                bindings,
            )
        }
        PatternKind::Tuple(elems) => {
            // The scrutinee must be a tuple of the same arity. Build a
            // tuple of fresh inference vars (or pre-existing element
            // types if scrutinee already concretely tuples), unify with
            // the scrutinee, then recurse on each pair.
            let mut elem_tys: Vec<InferType> = Vec::new();
            let mut k = 0;
            while k < elems.len() {
                let v = ctx.subst.fresh_var();
                elem_tys.push(InferType::Var(v));
                k += 1;
            }
            let tuple_ty = InferType::Tuple(infer_vec_clone(&elem_tys));
            ctx.subst.unify(
                scrutinee_ty,
                &tuple_ty,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &pattern.span,
                ctx.current_file,
            )?;
            let mut k = 0;
            while k < elems.len() {
                check_pattern(ctx, &elems[k], &elem_tys[k], bindings)?;
                k += 1;
            }
            Ok(())
        }
        PatternKind::Ref { inner, mutable } => {
            // Build `&T` (or `&mut T`) over a fresh inner var, unify with scrutinee, recurse.
            let inner_var = ctx.subst.fresh_var();
            let ref_ty = InferType::Ref {
                inner: Box::new(InferType::Var(inner_var)),
                mutable: *mutable,
                lifetime: LifetimeRepr::Inferred(0),
            };
            ctx.subst.unify(
                scrutinee_ty,
                &ref_ty,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &pattern.span,
                ctx.current_file,
            )?;
            check_pattern(ctx, inner, &InferType::Var(inner_var), bindings)
        }
        PatternKind::Or(alts) => {
            if alts.is_empty() {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "or-pattern with no alternatives".to_string(),
                    span: pattern.span.copy(),
                });
            }
            // Each alternative must bind the same set of names with
            // unifiable types. Walk the first alt to establish the
            // bindings, then check each subsequent alt against the
            // same set.
            let mark = bindings.len();
            check_pattern(ctx, &alts[0], scrutinee_ty, bindings)?;
            // Snapshot the first alt's bindings (name → type).
            let first_alt_bindings: Vec<(String, InferType, Span, bool)> = bindings[mark..]
                .iter()
                .map(|(n, t, s, m)| (n.clone(), infer_clone(t), s.copy(), *m))
                .collect();
            // Roll back; each alt independently produces the same set.
            bindings.truncate(mark);
            // Re-check first alt now that we know its bindings, and
            // unify with each subsequent alt's bindings.
            let mut k = 0;
            while k < alts.len() {
                let alt_mark = bindings.len();
                check_pattern(ctx, &alts[k], scrutinee_ty, bindings)?;
                if bindings.len() - alt_mark != first_alt_bindings.len() {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: "or-pattern alternatives must bind the same names".to_string(),
                        span: alts[k].span.copy(),
                    });
                }
                let alt_bindings: Vec<(String, InferType, Span, bool)> = bindings[alt_mark..]
                    .iter()
                    .map(|(n, t, s, m)| (n.clone(), infer_clone(t), s.copy(), *m))
                    .collect();
                // Match each alt's binding to the first alt's by name and unify types.
                let mut j = 0;
                while j < alt_bindings.len() {
                    let mut found = false;
                    let mut idx = 0;
                    while idx < first_alt_bindings.len() {
                        if first_alt_bindings[idx].0 == alt_bindings[j].0 {
                            ctx.subst.unify(
                                &alt_bindings[j].1,
                                &first_alt_bindings[idx].1,
                                ctx.traits,
                                ctx.type_params,
                                ctx.type_param_bounds,
                                &alt_bindings[j].2,
                                ctx.current_file,
                            )?;
                            found = true;
                            break;
                        }
                        idx += 1;
                    }
                    if !found {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message: format!(
                                "or-pattern alternatives must bind the same names; `{}` not in first alternative",
                                alt_bindings[j].0
                            ),
                            span: alt_bindings[j].2.copy(),
                        });
                    }
                    j += 1;
                }
                bindings.truncate(alt_mark);
                k += 1;
            }
            // After per-alt verification, install the canonical bindings once.
            let mut k = 0;
            while k < first_alt_bindings.len() {
                bindings.push((
                    first_alt_bindings[k].0.clone(),
                    infer_clone(&first_alt_bindings[k].1),
                    first_alt_bindings[k].2.copy(),
                    first_alt_bindings[k].3,
                ));
                k += 1;
            }
            Ok(())
        }
        PatternKind::Range { lo, hi } => {
            let resolved = ctx.subst.substitute(scrutinee_ty);
            match resolved {
                InferType::Int(_) | InferType::Var(_) => {}
                other => {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: format!(
                            "range pattern but scrutinee has type `{}`",
                            infer_to_string(&other)
                        ),
                        span: pattern.span.copy(),
                    });
                }
            }
            if lo > hi {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("range pattern: lower bound {} exceeds upper bound {}", lo, hi),
                    span: pattern.span.copy(),
                });
            }
            // Pin scrutinee var to integer class via fresh-int unification.
            let v = ctx.subst.fresh_int();
            ctx.subst.unify(
                scrutinee_ty,
                &InferType::Var(v),
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &pattern.span,
                ctx.current_file,
            )?;
            Ok(())
        }
        PatternKind::At { name, name_span, inner } => {
            // `name @ inner`: the at-binding follows real-Rust convention
            // and is non-mut by default (would require `mut name @ ...`,
            // which the parser doesn't accept). Treat as immutable.
            bindings.push((
                name.clone(),
                infer_clone(scrutinee_ty),
                name_span.copy(),
                false,
            ));
            check_pattern(ctx, inner, scrutinee_ty, bindings)
        }
    }
}

fn check_variant_tuple_pattern(
    ctx: &mut CheckCtx,
    path: &Path,
    elems: &Vec<Pattern>,
    scrutinee_ty: &InferType,
    span: &Span,
    bindings: &mut Vec<(String, InferType, Span, bool)>,
) -> Result<(), Error> {
    let raw_segs: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
    let (enum_path, disc) = match lookup_variant_path(
        ctx.enums,
        ctx.reexports,
        &ctx.use_scope,
        ctx.current_module,
        &raw_segs,
    ) {
        Some(x) => x,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unknown variant `{}` in pattern",
                    segments_to_string(&path.segments)
                ),
                span: span.copy(),
            });
        }
    };
    let entry = enum_lookup(ctx.enums, &enum_path).expect("variant lookup returned a real enum");
    if !is_visible_from(
        &type_defining_module(&entry.path),
        entry.is_pub,
        ctx.current_module,
    ) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("enum `{}` is private", place_to_string(&entry.path)),
            span: span.copy(),
        });
    }
    let variant = &entry.variants[disc];
    // Allocate fresh inference vars for the enum's type-params so we can
    // unify the scrutinee with this enum's type.
    let mut type_var_ids: Vec<u32> = Vec::with_capacity(entry.type_params.len());
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut k = 0;
    while k < entry.type_params.len() {
        let v = ctx.subst.fresh_var();
        type_var_ids.push(v);
        env.push((entry.type_params[k].clone(), InferType::Var(v)));
        k += 1;
    }
    let mut type_args_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < type_var_ids.len() {
        type_args_infer.push(InferType::Var(type_var_ids[k]));
        k += 1;
    }
    let enum_infer = InferType::Enum {
        path: clone_path(&entry.path),
        type_args: type_args_infer,
        lifetime_args: Vec::new(),
    };
    ctx.subst.unify(
        scrutinee_ty,
        &enum_infer,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        span,
        ctx.current_file,
    )?;
    // Validate payload shape and recurse.
    match &variant.payload {
        VariantPayloadResolved::Unit => {
            if !elems.is_empty() {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "variant `{}::{}` is a unit variant; pattern must be `{}::{}` (no parens)",
                        place_to_string(&entry.path),
                        variant.name,
                        place_to_string(&entry.path),
                        variant.name
                    ),
                    span: span.copy(),
                });
            }
            Ok(())
        }
        VariantPayloadResolved::Tuple(types) => {
            if elems.len() != types.len() {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "variant `{}::{}` takes {} fields, got {} in pattern",
                        place_to_string(&entry.path),
                        variant.name,
                        types.len(),
                        elems.len()
                    ),
                    span: span.copy(),
                });
            }
            let payload_types: Vec<RType> = rtype_vec_clone(types);
            let mut k = 0;
            while k < elems.len() {
                let expected = infer_substitute(&rtype_to_infer(&payload_types[k]), &env);
                check_pattern(ctx, &elems[k], &expected, bindings)?;
                k += 1;
            }
            Ok(())
        }
        VariantPayloadResolved::Struct(_) => Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "variant `{}::{}` is a struct-shaped variant; use `{}::{} {{ ... }}`",
                place_to_string(&entry.path),
                variant.name,
                place_to_string(&entry.path),
                variant.name
            ),
            span: span.copy(),
        }),
    }
}

fn check_variant_struct_pattern(
    ctx: &mut CheckCtx,
    path: &Path,
    fields: &Vec<crate::ast::FieldPattern>,
    rest: bool,
    scrutinee_ty: &InferType,
    span: &Span,
    bindings: &mut Vec<(String, InferType, Span, bool)>,
) -> Result<(), Error> {
    let raw_segs: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
    // Try enum-variant resolution first; if that misses, fall through
    // to a struct-pattern lookup (matches `Point { x, y }` against a
    // bare struct scrutinee).
    let variant_match = lookup_variant_path(
        ctx.enums,
        ctx.reexports,
        &ctx.use_scope,
        ctx.current_module,
        &raw_segs,
    );
    let (enum_path, disc) = match variant_match {
        Some(x) => x,
        None => {
            return check_struct_pattern(
                ctx, path, fields, rest, scrutinee_ty, span, bindings,
            );
        }
    };
    let entry = enum_lookup(ctx.enums, &enum_path).expect("variant lookup returned a real enum");
    if !is_visible_from(
        &type_defining_module(&entry.path),
        entry.is_pub,
        ctx.current_module,
    ) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("enum `{}` is private", place_to_string(&entry.path)),
            span: span.copy(),
        });
    }
    let variant = &entry.variants[disc];
    let field_defs: Vec<RTypedField> = match &variant.payload {
        VariantPayloadResolved::Struct(fs) => {
            let mut out: Vec<RTypedField> = Vec::new();
            let mut k = 0;
            while k < fs.len() {
                out.push(RTypedField {
                    name: fs[k].name.clone(),
                    name_span: fs[k].name_span.copy(),
                    ty: rtype_clone(&fs[k].ty),
                    is_pub: fs[k].is_pub,
                });
                k += 1;
            }
            out
        }
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "variant `{}::{}` is not a struct-shaped variant",
                    place_to_string(&entry.path),
                    variant.name
                ),
                span: span.copy(),
            });
        }
    };
    let mut type_var_ids: Vec<u32> = Vec::with_capacity(entry.type_params.len());
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut k = 0;
    while k < entry.type_params.len() {
        let v = ctx.subst.fresh_var();
        type_var_ids.push(v);
        env.push((entry.type_params[k].clone(), InferType::Var(v)));
        k += 1;
    }
    let mut type_args_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < type_var_ids.len() {
        type_args_infer.push(InferType::Var(type_var_ids[k]));
        k += 1;
    }
    let enum_infer = InferType::Enum {
        path: clone_path(&entry.path),
        type_args: type_args_infer,
        lifetime_args: Vec::new(),
    };
    ctx.subst.unify(
        scrutinee_ty,
        &enum_infer,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        span,
        ctx.current_file,
    )?;
    let mut seen: Vec<bool> = vec![false; field_defs.len()];
    let mut k = 0;
    while k < fields.len() {
        let fp = &fields[k];
        let mut found: Option<usize> = None;
        let mut j = 0;
        while j < field_defs.len() {
            if field_defs[j].name == fp.name {
                found = Some(j);
                break;
            }
            j += 1;
        }
        let idx = match found {
            Some(idx) => idx,
            None => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "unknown field `{}` on variant `{}::{}`",
                        fp.name,
                        place_to_string(&entry.path),
                        variant.name
                    ),
                    span: fp.name_span.copy(),
                });
            }
        };
        if seen[idx] {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("duplicate field `{}` in variant pattern", fp.name),
                span: fp.name_span.copy(),
            });
        }
        seen[idx] = true;
        let expected = infer_substitute(&rtype_to_infer(&field_defs[idx].ty), &env);
        check_pattern(ctx, &fp.pattern, &expected, bindings)?;
        k += 1;
    }
    if !rest {
        let mut k = 0;
        while k < field_defs.len() {
            if !seen[k] {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "missing field `{}` in variant pattern (use `..` to ignore)",
                        field_defs[k].name
                    ),
                    span: span.copy(),
                });
            }
            k += 1;
        }
    }
    Ok(())
}

// `Point { x, y }` against a struct-typed scrutinee. Same shape as
// the variant-struct path but resolves through the struct table and
// unifies the scrutinee with `RType::Struct`.
fn check_struct_pattern(
    ctx: &mut CheckCtx,
    path: &Path,
    fields: &Vec<crate::ast::FieldPattern>,
    rest: bool,
    scrutinee_ty: &InferType,
    span: &Span,
    bindings: &mut Vec<(String, InferType, Span, bool)>,
) -> Result<(), Error> {
    let raw_segs: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
    let resolved_path = resolve_via_use_scopes(&raw_segs, &ctx.use_scope, |cand| {
        struct_lookup_resolved(ctx.structs, ctx.reexports, cand).is_some()
    })
    .unwrap_or_else(|| {
        let mut full = clone_path(ctx.current_module);
        let mut k = 0;
        while k < raw_segs.len() {
            full.push(raw_segs[k].clone());
            k += 1;
        }
        full
    });
    let entry = match struct_lookup_resolved(ctx.structs, ctx.reexports, &resolved_path) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unknown struct or variant `{}` in pattern",
                    segments_to_string(&path.segments)
                ),
                span: span.copy(),
            });
        }
    };
    if !is_visible_from(
        &type_defining_module(&entry.path),
        entry.is_pub,
        ctx.current_module,
    ) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("struct `{}` is private", place_to_string(&entry.path)),
            span: span.copy(),
        });
    }
    let canon_path = clone_path(&entry.path);
    let field_defs: Vec<RTypedField> = {
        let mut out: Vec<RTypedField> = Vec::new();
        let mut k = 0;
        while k < entry.fields.len() {
            out.push(RTypedField {
                name: entry.fields[k].name.clone(),
                name_span: entry.fields[k].name_span.copy(),
                ty: rtype_clone(&entry.fields[k].ty),
                is_pub: entry.fields[k].is_pub,
            });
            k += 1;
        }
        out
    };
    let type_param_names: Vec<String> = entry.type_params.clone();
    // Allocate fresh inference vars for the struct's type-params.
    let mut type_var_ids: Vec<u32> = Vec::with_capacity(type_param_names.len());
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut k = 0;
    while k < type_param_names.len() {
        let v = ctx.subst.fresh_var();
        type_var_ids.push(v);
        env.push((type_param_names[k].clone(), InferType::Var(v)));
        k += 1;
    }
    let mut type_args_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < type_var_ids.len() {
        type_args_infer.push(InferType::Var(type_var_ids[k]));
        k += 1;
    }
    let struct_infer = InferType::Struct {
        path: clone_path(&canon_path),
        type_args: type_args_infer,
        lifetime_args: Vec::new(),
    };
    ctx.subst.unify(
        scrutinee_ty,
        &struct_infer,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        span,
        ctx.current_file,
    )?;
    let mut seen: Vec<bool> = vec![false; field_defs.len()];
    let mut k = 0;
    while k < fields.len() {
        let fp = &fields[k];
        let mut found: Option<usize> = None;
        let mut j = 0;
        while j < field_defs.len() {
            if field_defs[j].name == fp.name {
                found = Some(j);
                break;
            }
            j += 1;
        }
        let idx = match found {
            Some(idx) => idx,
            None => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "unknown field `{}` on struct `{}`",
                        fp.name,
                        place_to_string(&canon_path)
                    ),
                    span: fp.name_span.copy(),
                });
            }
        };
        if seen[idx] {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("duplicate field `{}` in struct pattern", fp.name),
                span: fp.name_span.copy(),
            });
        }
        seen[idx] = true;
        let expected = infer_substitute(&rtype_to_infer(&field_defs[idx].ty), &env);
        check_pattern(ctx, &fp.pattern, &expected, bindings)?;
        k += 1;
    }
    if !rest {
        let mut k = 0;
        while k < field_defs.len() {
            if !seen[k] {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "missing field `{}` in struct pattern (use `..` to ignore)",
                        field_defs[k].name
                    ),
                    span: span.copy(),
                });
            }
            k += 1;
        }
    }
    Ok(())
}

// Exhaustiveness check. Walk the arms and verify the scrutinee type's
// values are fully covered. For enums, every variant must be matched
// (each by its own arm or a wildcard / binding); for booleans, both
// `true` and `false`; for tuples, recurse into each element; for
// integers, require a wildcard / binding; etc.
fn check_match_exhaustive(
    ctx: &CheckCtx,
    scrutinee_ty: &InferType,
    arms: &Vec<crate::ast::MatchArm>,
    span: &Span,
) -> Result<(), Error> {
    // Collect the patterns from unguarded arms only — a guarded arm
    // is conditional, so its pattern doesn't reliably cover its own
    // domain. Real Rust takes the same approach: exhaustiveness
    // analysis sees only unconditional arms.
    let pats: Vec<&Pattern> = arms
        .iter()
        .filter(|a| a.guard.is_none())
        .map(|a| &a.pattern)
        .collect();
    if exhausted(ctx, scrutinee_ty, &pats) {
        Ok(())
    } else {
        Err(Error {
            file: ctx.current_file.to_string(),
            message: "non-exhaustive match: not all values are covered".to_string(),
            span: span.copy(),
        })
    }
}

// Returns true if the union of `pats` covers every value of `ty`.
fn exhausted(ctx: &CheckCtx, ty: &InferType, pats: &Vec<&Pattern>) -> bool {
    use crate::ast::PatternKind;
    // If any pattern at this level is an unconditional matcher
    // (Wildcard, Ident binding, At-binding wrapping unconditional),
    // we're trivially exhausted.
    let mut k = 0;
    while k < pats.len() {
        if pat_is_unconditional(pats[k]) {
            return true;
        }
        k += 1;
    }
    let resolved = ctx.subst.substitute(ty);
    match &resolved {
        InferType::Bool => {
            let mut has_true = false;
            let mut has_false = false;
            let mut k = 0;
            while k < pats.len() {
                walk_bool_pat(pats[k], &mut has_true, &mut has_false);
                k += 1;
            }
            has_true && has_false
        }
        InferType::Tuple(elems) => {
            // Per-position exhaustiveness: collect each tuple pattern's
            // sub-patterns into per-position columns; require every
            // column to be exhaustive. This is conservative (it
            // doesn't model "if column 0 is `true`, column 1 must be
            // exhausted *given* column 0 was true"), but it's correct
            // — a successful match requires every position to match
            // independently, so per-column exhaustiveness implies
            // overall coverage.
            let mut sub_pats: Vec<Vec<&Pattern>> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                sub_pats.push(Vec::new());
                i += 1;
            }
            let mut k = 0;
            while k < pats.len() {
                gather_tuple_subpats(pats[k], &mut sub_pats);
                k += 1;
            }
            let mut i = 0;
            while i < elems.len() {
                if !exhausted(ctx, &elems[i], &sub_pats[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        InferType::Ref { inner, .. } => {
            // `&pat` is exhaustive over `&T` iff `pat` is exhaustive
            // over `T`. Collect every arm's inner ref-pattern.
            let mut inner_pats: Vec<&Pattern> = Vec::new();
            let mut k = 0;
            while k < pats.len() {
                gather_ref_inner_pats(pats[k], &mut inner_pats);
                k += 1;
            }
            exhausted(ctx, inner, &inner_pats)
        }
        InferType::Struct { path, type_args, .. } => {
            // Per-field exhaustiveness, like the tuple case but for
            // named fields. A struct scrutinee is covered iff every
            // field's column is exhaustive.
            let entry = match struct_lookup(ctx.structs, path) {
                Some(e) => e,
                None => return false,
            };
            let env = {
                let mut e: Vec<(String, RType)> = Vec::new();
                let mut k = 0;
                while k < entry.type_params.len() && k < type_args.len() {
                    e.push((
                        entry.type_params[k].clone(),
                        infer_to_rtype_for_check(&type_args[k]),
                    ));
                    k += 1;
                }
                e
            };
            let mut sub_pats: Vec<Vec<&Pattern>> = Vec::new();
            let mut field_names: Vec<String> = Vec::new();
            let mut i = 0;
            while i < entry.fields.len() {
                sub_pats.push(Vec::new());
                field_names.push(entry.fields[i].name.clone());
                i += 1;
            }
            let mut k = 0;
            while k < pats.len() {
                gather_struct_subpats(pats[k], &field_names, &mut sub_pats);
                k += 1;
            }
            let mut i = 0;
            while i < entry.fields.len() {
                let subst_ty = substitute_rtype(&entry.fields[i].ty, &env);
                let pos_ty = rtype_to_infer(&subst_ty);
                if !exhausted(ctx, &pos_ty, &sub_pats[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        InferType::Enum { path, type_args, .. } => {
            let entry = match enum_lookup(ctx.enums, path) {
                Some(e) => e,
                None => return false,
            };
            // Build the enum's type-arg env so we can substitute Param
            // slots in payload types when recursing into per-position
            // exhaustiveness.
            let env = {
                let mut e: Vec<(String, RType)> = Vec::new();
                let mut k = 0;
                while k < entry.type_params.len() && k < type_args.len() {
                    e.push((
                        entry.type_params[k].clone(),
                        infer_to_rtype_for_check(&type_args[k]),
                    ));
                    k += 1;
                }
                e
            };
            let mut v = 0;
            while v < entry.variants.len() {
                let variant_name = &entry.variants[v].name;
                if !variant_covered(
                    ctx,
                    pats,
                    &entry.path,
                    variant_name,
                    &entry.variants[v].payload,
                    &env,
                ) {
                    return false;
                }
                v += 1;
            }
            true
        }
        // Integers, refs, raw pointers, struct values, etc.: exhaustive
        // only via a wildcard / binding (already short-circuited above).
        _ => false,
    }
}

// Push each tuple pattern's per-position sub-patterns into the
// columns. Or-patterns and at-bindings descend; non-tuple patterns
// are skipped (typeck would have rejected them against a tuple
// scrutinee). Unconditional bindings/wildcards at the top level
// implicitly cover every position — represented by pushing the
// pattern itself into every column so the per-column exhaustiveness
// check sees an unconditional arm there.
fn gather_tuple_subpats<'p>(p: &'p Pattern, columns: &mut Vec<Vec<&'p Pattern>>) {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::Tuple(es) => {
            let mut i = 0;
            while i < es.len() && i < columns.len() {
                columns[i].push(&es[i]);
                i += 1;
            }
        }
        PatternKind::Or(alts) => {
            let mut k = 0;
            while k < alts.len() {
                gather_tuple_subpats(&alts[k], columns);
                k += 1;
            }
        }
        PatternKind::At { inner, .. } => gather_tuple_subpats(inner, columns),
        PatternKind::Wildcard | PatternKind::Binding { .. } => {
            let mut i = 0;
            while i < columns.len() {
                columns[i].push(p);
                i += 1;
            }
        }
        _ => {}
    }
}

// Per-field columns from struct patterns. A struct pattern with `..`
// or that omits a field is treated as unconditional for the missing
// fields (push the pattern itself there). Or-patterns / at-bindings
// descend; unconditional bindings / wildcards cover every column.
fn gather_struct_subpats<'p>(
    p: &'p Pattern,
    field_names: &Vec<String>,
    columns: &mut Vec<Vec<&'p Pattern>>,
) {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::VariantStruct { fields, rest, .. } => {
            let mut covered: Vec<bool> = vec![false; field_names.len()];
            let mut k = 0;
            while k < fields.len() {
                let mut idx: Option<usize> = None;
                let mut j = 0;
                while j < field_names.len() {
                    if field_names[j] == fields[k].name {
                        idx = Some(j);
                        break;
                    }
                    j += 1;
                }
                if let Some(i) = idx {
                    columns[i].push(&fields[k].pattern);
                    covered[i] = true;
                }
                k += 1;
            }
            if *rest {
                let mut i = 0;
                while i < covered.len() {
                    if !covered[i] {
                        columns[i].push(p);
                    }
                    i += 1;
                }
            }
        }
        PatternKind::Or(alts) => {
            let mut k = 0;
            while k < alts.len() {
                gather_struct_subpats(&alts[k], field_names, columns);
                k += 1;
            }
        }
        PatternKind::At { inner, .. } => gather_struct_subpats(inner, field_names, columns),
        PatternKind::Wildcard | PatternKind::Binding { .. } => {
            let mut i = 0;
            while i < columns.len() {
                columns[i].push(p);
                i += 1;
            }
        }
        _ => {}
    }
}

// Collect the inner-of-ref sub-patterns from each arm pattern. `&p`
// contributes `p`; or-patterns and at-bindings descend; everything
// else is skipped (typeck would have rejected it against a `&T`
// scrutinee).
fn gather_ref_inner_pats<'p>(p: &'p Pattern, out: &mut Vec<&'p Pattern>) {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::Ref { inner, .. } => out.push(inner.as_ref()),
        PatternKind::Or(alts) => {
            let mut k = 0;
            while k < alts.len() {
                gather_ref_inner_pats(&alts[k], out);
                k += 1;
            }
        }
        PatternKind::At { inner, .. } => gather_ref_inner_pats(inner, out),
        _ => {}
    }
}

fn pat_is_unconditional(p: &Pattern) -> bool {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::Wildcard | PatternKind::Binding { .. } => true,
        PatternKind::At { inner, .. } => pat_is_unconditional(inner),
        PatternKind::Or(alts) => alts.iter().any(|a| pat_is_unconditional(a)),
        // `&p` matches every `&T` when `p` is unconditional (mutable
        // pattern likewise matches every `&mut T`); `(p, q, ...)` over
        // a tuple is unconditional when every element is.
        PatternKind::Ref { inner, .. } => pat_is_unconditional(inner),
        PatternKind::Tuple(elems) => elems.iter().all(|e| pat_is_unconditional(e)),
        _ => false,
    }
}

fn walk_bool_pat(p: &Pattern, has_true: &mut bool, has_false: &mut bool) {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::LitBool(true) => *has_true = true,
        PatternKind::LitBool(false) => *has_false = true,
        PatternKind::Or(alts) => {
            let mut k = 0;
            while k < alts.len() {
                walk_bool_pat(&alts[k], has_true, has_false);
                k += 1;
            }
        }
        PatternKind::At { inner, .. } => walk_bool_pat(inner, has_true, has_false),
        _ => {
            if pat_is_unconditional(p) {
                *has_true = true;
                *has_false = true;
            }
        }
    }
}

fn variant_covered(
    ctx: &CheckCtx,
    pats: &Vec<&Pattern>,
    enum_path: &Vec<String>,
    variant_name: &str,
    payload: &VariantPayloadResolved,
    enum_env: &Vec<(String, RType)>,
) -> bool {
    use crate::ast::PatternKind;
    // Collect sub-patterns inside this variant from any matching arm,
    // and require the union to cover all payload values.
    let mut sub_pats_per_position: Vec<Vec<&Pattern>> = match payload {
        VariantPayloadResolved::Unit => Vec::new(),
        VariantPayloadResolved::Tuple(types) => {
            let mut out: Vec<Vec<&Pattern>> = Vec::new();
            let mut i = 0;
            while i < types.len() {
                out.push(Vec::new());
                i += 1;
            }
            out
        }
        VariantPayloadResolved::Struct(fields) => {
            let mut out: Vec<Vec<&Pattern>> = Vec::new();
            let mut i = 0;
            while i < fields.len() {
                out.push(Vec::new());
                i += 1;
            }
            out
        }
    };
    let mut covered_unconditional_for_this_variant = false;
    let mut k = 0;
    while k < pats.len() {
        if pat_is_unconditional(pats[k]) {
            covered_unconditional_for_this_variant = true;
            break;
        }
        gather_variant_subpats(
            pats[k],
            enum_path,
            variant_name,
            payload,
            &mut sub_pats_per_position,
            &mut covered_unconditional_for_this_variant,
        );
        k += 1;
    }
    if covered_unconditional_for_this_variant {
        return true;
    }
    // For unit variant, no payload sub-patterns; covered iff some
    // arm's pattern explicitly named the variant (handled inside
    // gather_variant_subpats by setting the unconditional flag).
    // For tuple/struct variants, covered iff every position is
    // exhausted when restricted to this variant.
    match payload {
        VariantPayloadResolved::Unit => false,
        VariantPayloadResolved::Tuple(types) => {
            let mut i = 0;
            while i < types.len() {
                let pos_pats = &sub_pats_per_position[i];
                let subst_ty = substitute_rtype(&types[i], enum_env);
                let pos_ty = rtype_to_infer(&subst_ty);
                if !exhausted(ctx, &pos_ty, pos_pats) {
                    return false;
                }
                i += 1;
            }
            true
        }
        VariantPayloadResolved::Struct(fields) => {
            let mut i = 0;
            while i < fields.len() {
                let pos_pats = &sub_pats_per_position[i];
                let subst_ty = substitute_rtype(&fields[i].ty, enum_env);
                let pos_ty = rtype_to_infer(&subst_ty);
                if !exhausted(ctx, &pos_ty, pos_pats) {
                    return false;
                }
                i += 1;
            }
            true
        }
    }
}

fn gather_variant_subpats<'p>(
    p: &'p Pattern,
    enum_path: &Vec<String>,
    variant_name: &str,
    payload: &VariantPayloadResolved,
    sub_pats_per_position: &mut Vec<Vec<&'p Pattern>>,
    covered_unconditional: &mut bool,
) {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::VariantTuple { path, elems } => {
            // Match by name (last segment) — a full path comparison
            // would require us to canonicalize. Last-segment match is
            // adequate here because the scrutinee's enum type is fixed
            // by typeck so name collisions across enums can't happen.
            if path.segments.last().map(|s| s.name.as_str()) == Some(variant_name) {
                if let VariantPayloadResolved::Tuple(types) = payload {
                    if elems.len() == types.len()
                        && elems.iter().all(|e| pat_is_unconditional(e))
                    {
                        *covered_unconditional = true;
                        return;
                    }
                    let mut i = 0;
                    while i < elems.len() && i < sub_pats_per_position.len() {
                        sub_pats_per_position[i].push(&elems[i]);
                        i += 1;
                    }
                } else if let VariantPayloadResolved::Unit = payload {
                    if elems.is_empty() {
                        *covered_unconditional = true;
                    }
                }
            }
            let _ = enum_path;
        }
        PatternKind::VariantStruct { path, fields, rest } => {
            if path.segments.last().map(|s| s.name.as_str()) == Some(variant_name) {
                if let VariantPayloadResolved::Struct(field_defs) = payload {
                    if *rest || (fields.len() == field_defs.len()
                        && fields.iter().all(|f| pat_is_unconditional(&f.pattern)))
                    {
                        *covered_unconditional = true;
                        return;
                    }
                    let mut k = 0;
                    while k < fields.len() {
                        let mut idx = 0;
                        while idx < field_defs.len() {
                            if field_defs[idx].name == fields[k].name {
                                if idx < sub_pats_per_position.len() {
                                    sub_pats_per_position[idx].push(&fields[k].pattern);
                                }
                                break;
                            }
                            idx += 1;
                        }
                        k += 1;
                    }
                }
            }
        }
        PatternKind::Or(alts) => {
            let mut k = 0;
            while k < alts.len() {
                gather_variant_subpats(
                    &alts[k],
                    enum_path,
                    variant_name,
                    payload,
                    sub_pats_per_position,
                    covered_unconditional,
                );
                k += 1;
            }
        }
        PatternKind::At { inner, .. } => {
            gather_variant_subpats(
                inner,
                enum_path,
                variant_name,
                payload,
                sub_pats_per_position,
                covered_unconditional,
            );
        }
        PatternKind::Wildcard | PatternKind::Binding { .. } => {
            *covered_unconditional = true;
        }
        _ => {}
    }
}

// Builtin intrinsic check. The name encodes (type, op) — e.g.
// `u32_add`, `i64_eq`, `bool_and`, `bool_not`. Looks up the
// signature, verifies arg arity + types, returns the result type.
//
// Operation kinds:
//   - Arithmetic on int types (add, sub, mul, div, rem): (T, T) -> T.
//   - Comparison on int types (eq, ne, lt, le, gt, ge): (T, T) -> bool.
//   - Bool: and/or (bool, bool) -> bool; not (bool) -> bool;
//     eq/ne (bool, bool) -> bool.
fn check_builtin(
    ctx: &mut CheckCtx,
    name: &str,
    name_span: &Span,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    let sig = match builtin_signature(name) {
        Some(s) => s,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown builtin `¤{}`", name),
                span: name_span.copy(),
            });
        }
    };
    if args.len() != sig.params.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤{}` takes {} argument(s), got {}",
                name,
                sig.params.len(),
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let mut k = 0;
    while k < args.len() {
        let arg_ty = check_expr(ctx, &args[k])?;
        let expected = rtype_to_infer(&sig.params[k]);
        ctx.subst.unify(
            &arg_ty,
            &expected,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &args[k].span,
            ctx.current_file,
        )?;
        k += 1;
    }
    Ok(rtype_to_infer(&sig.result))
}

pub struct BuiltinSig {
    pub params: Vec<RType>,
    pub result: RType,
}

// Recognized builtins. The name's first segment is the type (one of
// `bool`, `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `u64`, `i64`,
// `usize`, `isize`); the rest is the operation. Returns `None` for
// any name we don't recognize. 128-bit builtins aren't supported yet
// (operands flatten to two wasm scalars; lowering needs a small
// runtime sequence).
pub fn builtin_signature(name: &str) -> Option<BuiltinSig> {
    // Split on the last `_` to separate type prefix from op suffix.
    // The op is one of a small fixed set; everything before the op
    // is the type name (which contains no `_`).
    let ops = [
        "add", "sub", "mul", "div", "rem", "eq", "ne", "lt", "le",
        "gt", "ge", "and", "or", "not", "xor",
    ];
    let mut found_op: Option<&str> = None;
    let mut k = 0;
    while k < ops.len() {
        let op = ops[k];
        if name.len() > op.len() + 1 {
            let prefix_end = name.len() - op.len();
            if name.as_bytes()[prefix_end - 1] == b'_'
                && &name[prefix_end..] == op
            {
                found_op = Some(op);
                break;
            }
        }
        k += 1;
    }
    let op = found_op?;
    let ty_name = &name[..name.len() - op.len() - 1];
    let ty = match ty_name {
        "bool" => RType::Bool,
        _ => RType::Int(int_kind_from_name(ty_name)?),
    };
    // Bool ops:
    if matches!(ty, RType::Bool) {
        match op {
            "and" | "or" | "xor" | "eq" | "ne" => {
                return Some(BuiltinSig {
                    params: vec![RType::Bool, RType::Bool],
                    result: RType::Bool,
                });
            }
            "not" => {
                return Some(BuiltinSig {
                    params: vec![RType::Bool],
                    result: RType::Bool,
                });
            }
            _ => return None,
        }
    }
    // Int ops:
    let is_arith = matches!(op, "add" | "sub" | "mul" | "div" | "rem" | "and" | "or" | "xor");
    let is_cmp = matches!(op, "eq" | "ne" | "lt" | "le" | "gt" | "ge");
    if is_arith {
        return Some(BuiltinSig {
            params: vec![rtype_clone(&ty), rtype_clone(&ty)],
            result: ty,
        });
    }
    if is_cmp {
        return Some(BuiltinSig {
            params: vec![rtype_clone(&ty), ty],
            result: RType::Bool,
        });
    }
    None
}

// `recv.method(args)` resolution. Type-check the receiver, peel one layer of
// ref to find the underlying struct, look up `[StructPath, method_name]` in
// FuncTable, derive a `ReceiverAdjust` from the recv type vs the method's
// receiver type, type-check remaining args, and record the resolution for
// borrowck/codegen consumption.
// Symbolic method dispatch via a type-param's trait bounds. Used when
// `recv: T` (or `&T`/`&mut T`) and `T: Trait` is in scope. Verifies the
// trait declares the method, type-checks args against the trait method's
// signature, and stamps a `TraitDispatch` resolution for codegen to
// re-resolve at monomorphization.
fn check_method_call_symbolic(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    param_name: String,
    recv_through_mut_ref: bool,
) -> Result<InferType, Error> {
    // Find the param's index in ctx.type_params.
    let mut idx: Option<usize> = None;
    let mut i = 0;
    while i < ctx.type_params.len() {
        if ctx.type_params[i] == param_name {
            idx = Some(i);
            break;
        }
        i += 1;
    }
    let idx = match idx {
        Some(v) => v,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("type parameter `{}` not in scope", param_name),
                span: mc.method_span.copy(),
            });
        }
    };
    // Find traits that declare this method.
    let mut matching_traits: Vec<Vec<String>> = Vec::new();
    let bounds = if idx < ctx.type_param_bounds.len() {
        &ctx.type_param_bounds[idx]
    } else {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "no method `{}` on `{}` (no trait bound provides it)",
                mc.method, param_name
            ),
            span: mc.method_span.copy(),
        });
    };
    let mut bi = 0;
    while bi < bounds.len() {
        let closure = supertrait_closure(&bounds[bi], ctx.traits);
        let mut ci = 0;
        while ci < closure.len() {
            if let Some(trait_entry) = trait_lookup(ctx.traits, &closure[ci]) {
                let mut mi = 0;
                while mi < trait_entry.methods.len() {
                    if trait_entry.methods[mi].name == mc.method {
                        let already = matching_traits.iter().any(|t| path_eq(t, &closure[ci]));
                        if !already {
                            matching_traits.push(clone_path(&closure[ci]));
                        }
                        break;
                    }
                    mi += 1;
                }
            }
            ci += 1;
        }
        bi += 1;
    }
    if matching_traits.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "no method `{}` on `{}` (no trait bound provides it)",
                mc.method, param_name
            ),
            span: mc.method_span.copy(),
        });
    }
    if matching_traits.len() > 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "ambiguous method `{}` on `{}`: multiple trait bounds provide it",
                mc.method, param_name
            ),
            span: mc.method_span.copy(),
        });
    }
    let trait_full = matching_traits.into_iter().next().unwrap();
    let trait_entry = trait_lookup(ctx.traits, &trait_full).expect("looked up above");
    // Find the matching method declaration to extract param count + return.
    let mut mi = 0;
    while mi < trait_entry.methods.len() {
        if trait_entry.methods[mi].name == mc.method {
            break;
        }
        mi += 1;
    }
    let trait_method = &trait_entry.methods[mi];
    let trait_param_types = rtype_vec_clone(&trait_method.param_types);
    let trait_return_type = trait_method.return_type.as_ref().map(rtype_clone);
    let trait_recv_shape = trait_method.receiver_shape;
    let trait_method_type_params: Vec<String> = trait_method.type_params.clone();
    // Drop the borrow into traits before mutating ctx.
    drop(trait_entry);
    let expected_arg_count = if trait_param_types.is_empty() {
        0
    } else {
        trait_param_types.len() - 1
    };
    if mc.args.len() != expected_arg_count {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected {}, got {}",
                mc.method,
                expected_arg_count,
                mc.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    // T2.5b: trait methods with their own type-params (`fn bar<U>(...)`)
    // get fresh inference vars per call. Optional turbofish
    // (`recv.bar::<u32>(...)`) pins them.
    let mut method_subst: Vec<(String, InferType)> = vec![(
        "Self".to_string(),
        InferType::Param(param_name.clone()),
    )];
    let mut method_type_var_ids: Vec<u32> = Vec::new();
    let mut tp = 0;
    while tp < trait_method_type_params.len() {
        let v = ctx.subst.fresh_var();
        method_subst.push((
            trait_method_type_params[tp].clone(),
            InferType::Var(v),
        ));
        method_type_var_ids.push(v);
        tp += 1;
    }
    if !mc.turbofish_args.is_empty() {
        if mc.turbofish_args.len() != trait_method_type_params.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of type arguments to method `{}`: expected {}, got {}",
                    mc.method,
                    trait_method_type_params.len(),
                    mc.turbofish_args.len()
                ),
                span: mc.method_span.copy(),
            });
        }
        let mut t = 0;
        while t < mc.turbofish_args.len() {
            let user_rt = resolve_type(
                &mc.turbofish_args[t],
                ctx.current_module,
                ctx.structs,
                ctx.enums,
                ctx.self_target,
                ctx.type_params,
                &ctx.use_scope,
                ctx.reexports,
                ctx.current_file,
            )?;
            let user_infer = rtype_to_infer(&user_rt);
            ctx.subst.unify(
                &InferType::Var(method_type_var_ids[t]),
                &user_infer,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &mc.method_span,
                ctx.current_file,
            )?;
            t += 1;
        }
    }
    let mut expected_arg_infers: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < expected_arg_count {
        // param 0 is the receiver; remaining params start at index 1.
        let pt = &trait_param_types[k + 1];
        let infer = infer_substitute(&rtype_to_infer(pt), &method_subst);
        expected_arg_infers.push(infer);
        k += 1;
    }
    // Type-check each arg expression and unify with the trait method's
    // declared param type (after substitution).
    let mut k = 0;
    while k < mc.args.len() {
        let arg_ty = check_expr(ctx, &mc.args[k])?;
        ctx.subst.unify(
            &arg_ty,
            &expected_arg_infers[k],
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &mc.args[k].span,
            ctx.current_file,
        )?;
        k += 1;
    }
    // recv_type_infer for codegen: the receiver after adjust. For symbolic
    // dispatch we pass through the receiver's InferType as-is (Param or
    // Ref<Param>) — codegen substitutes Param at mono time.
    let recv_for_storage = if recv_through_mut_ref {
        InferType::Ref {
            inner: Box::new(InferType::Param(param_name.clone())),
            mutable: true,
            lifetime: LifetimeRepr::Inferred(0),
        }
    } else {
        // The original recv may have been `T` (consume) or `&T` (shared
        // ref); we surface T in either case here. Codegen reapplies the
        // appropriate adjustment.
        InferType::Param(param_name.clone())
    };
    // T2.5: derive recv_adjust from the trait method's declared receiver
    // shape. For symbolic dispatch through a `Param(T)` recv, this drives
    // codegen autoref:
    //   trait method `&self` + recv owned T  → BorrowImm
    //   trait method `&self` + recv `&T`     → ByRef
    //   trait method `&mut self` + recv `&mut T` → ByRef
    // Mismatches (e.g. recv `&T` for a `&mut self` method) are rejected.
    let recv_adjust = match trait_recv_shape {
        Some(TraitReceiverShape::Move) => {
            if recv_through_mut_ref {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of `&mut {}` to call `{}` (which takes `self` by value)",
                        param_name, mc.method
                    ),
                    span: mc.method_span.copy(),
                });
            }
            ReceiverAdjust::Move
        }
        Some(TraitReceiverShape::BorrowImm) => {
            if recv_through_mut_ref {
                ReceiverAdjust::ByRef
            } else {
                ReceiverAdjust::BorrowImm
            }
        }
        Some(TraitReceiverShape::BorrowMut) => {
            if recv_through_mut_ref {
                ReceiverAdjust::ByRef
            } else {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot call `&mut self` method `{}` on owned `{}` (no implicit borrow)",
                        mc.method, param_name
                    ),
                    span: mc.method_span.copy(),
                });
            }
        }
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "method `{}` is an associated function with no receiver",
                    mc.method
                ),
                span: mc.method_span.copy(),
            });
        }
    };
    // Stash the method-level type-vars on the resolution; they'll be
    // resolved (substituted) at PendingMethodCall finalization. For
    // symbolic dispatch these are *only* the method's own params (impl-
    // level params come from `solve_impl` at codegen time), so the
    // length equals `trait_method.type_params.len()`.
    let mut type_arg_infers: Vec<InferType> = Vec::new();
    let mut t = 0;
    while t < method_type_var_ids.len() {
        type_arg_infers.push(InferType::Var(method_type_var_ids[t]));
        t += 1;
    }
    ctx.method_resolutions[call_expr.id as usize] = Some(PendingMethodCall {
        callee_idx: 0,
        callee_path: Vec::new(),
        recv_adjust,
        ret_borrows_receiver: false,
        template_idx: None,
        type_arg_infers,
        trait_dispatch: Some(PendingTraitDispatch {
            trait_path: clone_path(&trait_full),
            method_name: mc.method.clone(),
            recv_type_infer: recv_for_storage,
        }),
    });
    // Return type comes from the trait method's declared signature with
    // Self + method-level type-params substituted. Tail-less methods
    // return `()`.
    let _ = call_expr;
    let infer = match &trait_return_type {
        Some(ret_rt) => infer_substitute(&rtype_to_infer(ret_rt), &method_subst),
        None => InferType::Tuple(Vec::new()),
    };
    Ok(infer)
}

fn check_method_call(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
) -> Result<InferType, Error> {
    let recv_ty = check_expr(ctx, &mc.receiver)?;
    let resolved_recv = ctx.subst.substitute(&recv_ty);
    // T2: handle symbolic dispatch when recv is `Param(T)` — find the
    // method via T's trait bounds.
    if let InferType::Param(name) = &resolved_recv {
        return check_method_call_symbolic(ctx, mc, call_expr, name.clone(), false);
    }
    if let InferType::Ref { inner, mutable, .. } = &resolved_recv {
        if let InferType::Param(name) = inner.as_ref() {
            return check_method_call_symbolic(
                ctx,
                mc,
                call_expr,
                name.clone(),
                *mutable,
            );
        }
    }
    // T2.6: classify recv into recv_kind plus a full + peeled InferType.
    // Dispatch tries `try_match` against the full recv first (handles
    // blanket impls like `impl<T> Show for &T` and primitive-target
    // impls like `impl Show for u32`); when that fails for a Ref recv,
    // it retries against the peeled inner (handles inherent impls
    // and struct-target trait impls invoked via autoref).
    let (recv_kind, recv_full, recv_peeled): (RecvShape, InferType, Option<InferType>) =
        match &resolved_recv {
            InferType::Ref { inner, mutable, .. } => {
                let kind = if *mutable {
                    RecvShape::MutRef
                } else {
                    RecvShape::SharedRef
                };
                let peeled = infer_clone(inner.as_ref());
                (kind, infer_clone(&resolved_recv), Some(peeled))
            }
            _ => (RecvShape::Owned, infer_clone(&resolved_recv), None),
        };
    // Pull out struct_path + recv_type_args for downstream env-building
    // (only meaningful when the matched impl_target is struct-shaped).
    let struct_path: Vec<String> = match &resolved_recv {
        InferType::Struct { path, .. } => clone_path(path),
        InferType::Ref { inner, .. } => match inner.as_ref() {
            InferType::Struct { path, .. } => clone_path(path),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let recv_type_args: Vec<InferType> = match &resolved_recv {
        InferType::Struct { type_args, .. } => infer_vec_clone(type_args),
        InferType::Ref { inner, .. } => match inner.as_ref() {
            InferType::Struct { type_args, .. } => infer_vec_clone(type_args),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut method_path = clone_path(&struct_path);
    method_path.push(mc.method.clone());
    let candidates = find_method_candidates(ctx.funcs, &mc.method);
    if candidates.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("no method `{}` on `{}`", mc.method, infer_to_string(&recv_full)),
            span: mc.method_span.copy(),
        });
    }
    // Per matched candidate we record a `match_tier`, lower = more
    // direct (mirrors Rust's deref-probe sequence; see "Method
    // dispatch" in CLAUDE.md):
    //   0 — direct: pattern matches recv_full as-is.
    //   1 — pattern-side autoref: pattern is `&T` / `&mut T`; the
    //       pattern's inner matches recv_full. Corresponds to Rust
    //       autoref'ing the receiver to align with the impl pattern.
    //   2 — recv-side peel: recv is a Ref and the pattern matches the
    //       peeled inner. Corresponds to autoderef.
    let mut matched: Vec<(
        MethodCandidate,
        Vec<(String, InferType)>,
        Vec<(InferType, InferType)>,
        u8,
    )> = Vec::new();
    for cand in &candidates {
        let impl_target_opt: Option<RType> = match cand {
            MethodCandidate::Direct(i) => {
                ctx.funcs.entries[*i].impl_target.as_ref().map(rtype_clone)
            }
            MethodCandidate::Template(i) => {
                ctx.funcs.templates[*i].impl_target.as_ref().map(rtype_clone)
            }
        };
        let pat = match &impl_target_opt {
            Some(p) => p,
            None => continue,
        };
        // Tier 0: full recv.
        let mut env_so_far: Vec<(String, InferType)> = Vec::new();
        let mut pending: Vec<(InferType, InferType)> = Vec::new();
        let mut ok = try_match_against_infer(
            pat,
            &recv_full,
            &ctx.subst,
            &mut env_so_far,
            &mut pending,
        );
        let mut match_tier: u8 = 0;
        if !ok {
            // Tier 1: pattern-side autoref. Only meaningful when the
            // pattern is shaped `&T` / `&mut T`; we peel that off and
            // match its inner against recv_full.
            if let RType::Ref { inner: pat_inner, .. } = pat {
                env_so_far = Vec::new();
                pending = Vec::new();
                ok = try_match_against_infer(
                    pat_inner,
                    &recv_full,
                    &ctx.subst,
                    &mut env_so_far,
                    &mut pending,
                );
                if ok {
                    match_tier = 1;
                }
            }
        }
        if !ok {
            // Tier 2: recv-side peel.
            if let Some(peeled) = &recv_peeled {
                env_so_far = Vec::new();
                pending = Vec::new();
                ok = try_match_against_infer(
                    pat,
                    peeled,
                    &ctx.subst,
                    &mut env_so_far,
                    &mut pending,
                );
                if ok {
                    match_tier = 2;
                }
            }
        }
        if ok {
            matched.push((*cand, env_so_far, pending, match_tier));
        }
    }
    if matched.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("no method `{}` on `{}`", mc.method, infer_to_string(&recv_full)),
            span: mc.method_span.copy(),
        });
    }
    // T2.6.5: when more than one candidate type-matched, filter by
    // recv-adjust validity. Drop candidates whose `derive_recv_adjust`
    // would error (e.g. method takes `self` by value but recv is a
    // borrow). Only declare ambiguity if multiple still survive.
    if matched.len() > 1 {
        let mut adjust_valid: Vec<usize> = Vec::new();
        let mut idx = 0;
        while idx < matched.len() {
            let cand = &matched[idx].0;
            let recv_param: RType = match cand {
                MethodCandidate::Direct(i) => {
                    rtype_clone(&ctx.funcs.entries[*i].param_types[0])
                }
                MethodCandidate::Template(i) => {
                    rtype_clone(&ctx.funcs.templates[*i].param_types[0])
                }
            };
            if derive_recv_adjust(
                &recv_kind,
                &recv_param,
                ctx,
                &mc.receiver,
                &mc.method_span,
            )
            .is_ok()
            {
                adjust_valid.push(idx);
            }
            idx += 1;
        }
        if adjust_valid.is_empty() {
            // None of the candidates can adjust to the receiver shape.
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "no method `{}` callable on `{}`",
                    mc.method,
                    infer_to_string(&recv_full)
                ),
                span: mc.method_span.copy(),
            });
        }
        // Drop adjust-invalid candidates first.
        let valid_set: Vec<usize> = adjust_valid;
        matched = matched
            .into_iter()
            .enumerate()
            .filter_map(|(i, m)| if valid_set.contains(&i) { Some(m) } else { None })
            .collect();
        // If still ambiguous, prefer the minimum match_tier — Rust's
        // dispatch probes the receiver type as-is first, then autoref,
        // then deref. Stops at the first hit. Only a true overlap at
        // the same tier (e.g. unrelated traits supplying the same
        // method name on the same recv shape) declares ambiguity.
        if matched.len() > 1 {
            let mut min_tier: u8 = u8::MAX;
            let mut k = 0;
            while k < matched.len() {
                if matched[k].3 < min_tier {
                    min_tier = matched[k].3;
                }
                k += 1;
            }
            matched = matched.into_iter().filter(|m| m.3 == min_tier).collect();
            if matched.len() > 1 {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "ambiguous method `{}` on `{}`: multiple impls match",
                        mc.method,
                        infer_to_string(&recv_full)
                    ),
                    span: mc.method_span.copy(),
                });
            }
        }
    }
    let (chosen_cand, mut chosen_env, chosen_pending, _match_tier) =
        matched.into_iter().next().unwrap();
    // Commit the pending unifications discovered during pattern matching.
    let mut pi = 0;
    while pi < chosen_pending.len() {
        ctx.subst.unify(
            &chosen_pending[pi].0,
            &chosen_pending[pi].1,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &mc.receiver.span,
            ctx.current_file,
        )?;
        pi += 1;
    }
    let (
        mp_param_types,
        mp_return_type,
        mp_type_params,
        mp_callee_idx,
        mp_param_lifetimes,
        mp_ret_lifetime,
        mp_is_template,
        mp_template_idx,
    ) = match chosen_cand {
        MethodCandidate::Direct(idx) => {
            let entry = &ctx.funcs.entries[idx];
            (
                rtype_vec_clone(&entry.param_types),
                entry.return_type.as_ref().map(rtype_clone),
                Vec::new(),
                entry.idx,
                clone_param_lifetimes(&entry.param_lifetimes),
                entry.ret_lifetime.as_ref().map(lifetime_repr_clone),
                false,
                0usize,
            )
        }
        MethodCandidate::Template(idx) => {
            let t = &ctx.funcs.templates[idx];
            (
                rtype_vec_clone(&t.param_types),
                t.return_type.as_ref().map(rtype_clone),
                t.type_params.clone(),
                0u32,
                clone_param_lifetimes(&t.param_lifetimes),
                t.ret_lifetime.as_ref().map(lifetime_repr_clone),
                true,
                idx,
            )
        }
    };
    if mp_param_types.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "function `{}` is not a method (no `self` receiver)",
                place_to_string(&method_path)
            ),
            span: mc.method_span.copy(),
        });
    }
    // Build env: impl's type_params come from `chosen_env` (filled by
    // try_match against the receiver). Method's own type_params (the
    // trailing entries in `mp_type_params` after the impl's) get fresh
    // inference vars, optionally unified with turbofish.
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut method_type_var_ids: Vec<u32> = Vec::new();
    if mp_is_template {
        // First, copy chosen_env entries for impl-level params, in the
        // order of mp_type_params (so impl_type_param_count slots map
        // correctly).
        let impl_param_count = match chosen_cand {
            MethodCandidate::Template(idx) => ctx.funcs.templates[idx].impl_type_param_count,
            MethodCandidate::Direct(_) => 0,
        };
        let mut i = 0;
        while i < impl_param_count {
            // Find this name in chosen_env (try_match may have left it
            // unbound if the impl_target's pattern didn't reference it,
            // but that shouldn't happen for well-formed impls).
            let name = &mp_type_params[i];
            let mut found: Option<InferType> = None;
            let mut k = 0;
            while k < chosen_env.len() {
                if chosen_env[k].0 == *name {
                    found = Some(infer_clone(&chosen_env[k].1));
                    break;
                }
                k += 1;
            }
            let bound = match found {
                Some(v) => v,
                None => InferType::Var(ctx.subst.fresh_var()),
            };
            env.push((name.clone(), bound));
            method_type_var_ids.push(0);
            i += 1;
        }
        // Then method's own params: fresh vars, possibly unified with turbofish.
        let method_own_count = mp_type_params.len() - impl_param_count;
        let mut i = 0;
        while i < method_own_count {
            let v = ctx.subst.fresh_var();
            env.push((
                mp_type_params[impl_param_count + i].clone(),
                InferType::Var(v),
            ));
            method_type_var_ids.push(v);
            i += 1;
        }
        if !mc.turbofish_args.is_empty() {
            if mc.turbofish_args.len() != method_own_count {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "wrong number of type arguments to method `{}`: expected {}, got {}",
                        mc.method,
                        method_own_count,
                        mc.turbofish_args.len()
                    ),
                    span: mc.method_span.copy(),
                });
            }
            let mut k = 0;
            while k < mc.turbofish_args.len() {
                let user_rt = resolve_type(
                    &mc.turbofish_args[k],
                    ctx.current_module,
                    ctx.structs,
                    ctx.enums,
                    ctx.self_target,
                    ctx.type_params,
                    &ctx.use_scope,
                    ctx.reexports,
                    ctx.current_file,
                )?;
                let user_infer = rtype_to_infer(&user_rt);
                let var_id = method_type_var_ids[impl_param_count + k];
                ctx.subst.unify(
                    &InferType::Var(var_id),
                    &user_infer,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    &mc.turbofish_args[k].span,
                    ctx.current_file,
                )?;
                k += 1;
            }
        }
        // Suppress unused-warning during transition — remove later.
        let _ = &mut chosen_env;
    } else if !mc.turbofish_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("method `{}` is not generic", mc.method),
            span: mc.method_span.copy(),
        });
    }
    let recv_param = &mp_param_types[0];
    let recv_adjust = derive_recv_adjust(&recv_kind, recv_param, ctx, &mc.receiver, &mc.method_span)?;
    let expected_arg_count = mp_param_types.len() - 1;
    if mc.args.len() != expected_arg_count {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected {}, got {}",
                mc.method,
                expected_arg_count,
                mc.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let callee_idx = mp_callee_idx;
    // Phase D: a return ref's lifetime may match more than one param when
    // the user writes `fn longest<'a>(&'a self, other: &'a u32) -> &'a u32`.
    // For method-call propagation we still surface only the
    // receiver-vs-not bit (`ret_borrows_receiver`) — non-receiver source
    // borrows fall through normal call-slot handling, same as today.
    let callee_ret_sources: Vec<usize> = match &mp_ret_lifetime {
        Some(rt_lt) => find_lifetime_source(&mp_param_lifetimes, rt_lt),
        None => Vec::new(),
    };
    let mut method_param_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < mp_param_types.len() {
        let raw = rtype_to_infer(&mp_param_types[k]);
        let subst = if mp_is_template {
            infer_substitute(&raw, &env)
        } else {
            raw
        };
        method_param_infer.push(subst);
        k += 1;
    }
    let return_infer: Option<InferType> = match &mp_return_type {
        Some(rt) => {
            let raw = rtype_to_infer(rt);
            Some(if mp_is_template {
                infer_substitute(&raw, &env)
            } else {
                raw
            })
        }
        None => None,
    };
    // Build the type-arg env (only meaningful for templates).
    let type_arg_infers: Vec<InferType> = if mp_is_template {
        let mut v: Vec<InferType> = Vec::new();
        let mut i = 0;
        while i < mp_type_params.len() {
            v.push(infer_clone(&env[i].1));
            i += 1;
        }
        v
    } else {
        Vec::new()
    };
    // Record the resolution at this MethodCall's NodeId.
    let template_idx_opt = if mp_is_template { Some(mp_template_idx) } else { None };
    ctx.method_resolutions[call_expr.id as usize] = Some(PendingMethodCall {
        callee_idx,
        callee_path: clone_path(&method_path),
        recv_adjust,
        ret_borrows_receiver: false,
        template_idx: template_idx_opt,
        type_arg_infers,
        trait_dispatch: None,
    });
    // Type-check remaining args against method's params[1..].
    let mut i = 0;
    while i < mc.args.len() {
        let arg_ty = check_expr(ctx, &mc.args[i])?;
        ctx.subst.unify(
            &arg_ty,
            &method_param_infer[i + 1],
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &mc.args[i].span,
            ctx.current_file,
        )?;
        i += 1;
    }
    // Record whether this call's result borrow should be attributed to the
    // receiver place (for borrowck propagation through ref-returning methods).
    let ret_borrows_recv = callee_ret_sources.iter().any(|&i| i == 0)
        && matches!(
            ctx.method_resolutions[call_expr.id as usize].as_ref().unwrap().recv_adjust,
            ReceiverAdjust::BorrowImm
                | ReceiverAdjust::BorrowMut
                | ReceiverAdjust::ByRef
        );
    ctx.method_resolutions[call_expr.id as usize].as_mut().unwrap().ret_borrows_receiver = ret_borrows_recv;
    let _ = mc;
    Ok(match return_infer {
        Some(rt) => rt,
        None => InferType::Tuple(Vec::new()),
    })
}

enum RecvShape {
    Owned,
    SharedRef,
    MutRef,
}

fn derive_recv_adjust(
    recv_kind: &RecvShape,
    recv_param: &RType,
    ctx: &CheckCtx,
    recv_expr: &Expr,
    method_span: &Span,
) -> Result<ReceiverAdjust, Error> {
    match recv_param {
        RType::Ref {
            mutable: param_mut,
            ..
        } => {
            // Method takes `&Self` or `&mut Self`.
            match (recv_kind, param_mut) {
                (RecvShape::Owned, false) => Ok(ReceiverAdjust::BorrowImm),
                (RecvShape::Owned, true) => {
                    if !is_mutable_place(ctx, recv_expr) {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message:
                                "cannot call `&mut self` method on an immutable receiver"
                                    .to_string(),
                            span: method_span.copy(),
                        });
                    }
                    Ok(ReceiverAdjust::BorrowMut)
                }
                (RecvShape::SharedRef, false) => Ok(ReceiverAdjust::ByRef),
                (RecvShape::SharedRef, true) => Err(Error {
                    file: ctx.current_file.to_string(),
                    message:
                        "cannot call `&mut self` method through a shared reference"
                            .to_string(),
                    span: method_span.copy(),
                }),
                (RecvShape::MutRef, false) => Ok(ReceiverAdjust::ByRef),
                (RecvShape::MutRef, true) => Ok(ReceiverAdjust::ByRef),
            }
        }
        // T2.6: any non-Ref recv_param (Struct, Int, RawPtr, Param)
        // means "method takes Self by value" — receiver moves in.
        _ => match recv_kind {
            RecvShape::Owned => Ok(ReceiverAdjust::Move),
            _ => Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot move out of borrow to call `{}` (which takes `self` by value)",
                    token_method_name(recv_expr)
                ),
                span: method_span.copy(),
            }),
        },
    }
}

fn token_method_name(_recv: &Expr) -> &'static str {
    // Placeholder: we only use this in an error message that's about the
    // receiver, not the method itself.
    "this method"
}

// A receiver is a "mutable place" if it's a Var bound to a `mut` local, or a
// FieldAccess chain rooted at one — same rule as for `&mut place` borrows.
fn is_mutable_place(ctx: &CheckCtx, expr: &Expr) -> bool {
    let chain = match extract_place_for_assign(expr) {
        Some(c) => c,
        None => return false,
    };
    let mut i = ctx.locals.len();
    while i > 0 {
        i -= 1;
        if ctx.locals[i].name == chain[0] {
            // Owned `mut` binding, or a `&mut T` binding.
            if ctx.locals[i].mutable {
                return true;
            }
            let resolved = ctx.subst.substitute(&ctx.locals[i].ty);
            return matches!(resolved, InferType::Ref { mutable: true, .. });
        }
    }
    false
}

// `*p` reads the pointed-to value. Result type = inner of the ref/raw-ptr.
// We don't enforce Copy here — that check kicks in only when the deref is
// USED as a value (the caller can decide). `(*p).field` access still applies
// the Copy rule via `check_field_access`'s "through reference" branch.
// Walk an expression as a place (memory location). Used for the inner of
// `&...` / `&mut...` so the "Copy through ref" rule on intermediate field
// accesses doesn't fire — borrowing a non-Copy place is fine; only reading
// a place into a value moves out. Place expressions are: `Var`, `FieldAccess`
// on a place, `Deref` on a value (the value side is a ref/raw-ptr). For any
// other shape (e.g., `&call()`), falls back to `check_expr` (treats the inner
// as a value — borrowck won't track such borrows).
fn check_place_expr(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
    match &expr.kind {
        ExprKind::Var(_)
        | ExprKind::FieldAccess(_)
        | ExprKind::Deref(_)
        | ExprKind::TupleIndex { .. } => {
            let ty = check_place_inner(ctx, expr)?;
            ctx.expr_infer_types[expr.id as usize] = Some(infer_clone(&ty));
            Ok(ty)
        }
        _ => check_expr(ctx, expr),
    }
}

fn check_place_inner(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
    match &expr.kind {
        ExprKind::Var(name) => {
            let mut i = ctx.locals.len();
            while i > 0 {
                i -= 1;
                if ctx.locals[i].name == *name {
                    return Ok(infer_clone(&ctx.locals[i].ty));
                }
            }
            Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown variable: `{}`", name),
                span: expr.span.copy(),
            })
        }
        ExprKind::FieldAccess(fa) => {
            let base_ty = check_place_expr(ctx, &fa.base)?;
            let resolved = ctx.subst.substitute(&base_ty);
            // Auto-deref one level through `&T` / `&mut T` (matches the
            // value-position field-access behavior).
            let (struct_path, struct_type_args) = match resolved {
                InferType::Struct { path, type_args, .. } => (path, type_args),
                InferType::Ref { inner, .. } => match *inner {
                    InferType::Struct { path, type_args, .. } => (path, type_args),
                    _ => {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message: "field access on non-struct value".to_string(),
                            span: fa.base.span.copy(),
                        });
                    }
                },
                _ => {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: "field access on non-struct value".to_string(),
                        span: fa.base.span.copy(),
                    });
                }
            };
            let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
            let mut i = 0;
            while i < entry.fields.len() {
                if entry.fields[i].name == fa.field {
                    let env = build_infer_env(&entry.type_params, &struct_type_args);
                    let field_raw = rtype_to_infer(&entry.fields[i].ty);
                    return Ok(infer_substitute(&field_raw, &env));
                }
                i += 1;
            }
            Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "no field `{}` on `{}`",
                    fa.field,
                    place_to_string(&struct_path)
                ),
                span: fa.field_span.copy(),
            })
        }
        ExprKind::Deref(inner) => {
            // The inner of a Deref is a value (a ref or raw-ptr that holds
            // the place's address). Use check_expr to type-check the value.
            let inner_ty = check_expr(ctx, inner)?;
            let resolved = ctx.subst.substitute(&inner_ty);
            match resolved {
                InferType::Ref { inner, .. } => Ok(*inner),
                InferType::RawPtr { inner, .. } => Ok(*inner),
                other => Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot dereference `{}` — only references and raw pointers can be dereferenced",
                        infer_to_string(&other)
                    ),
                    span: expr.span.copy(),
                }),
            }
        }
        ExprKind::TupleIndex { base, index, index_span } => {
            let base_ty = check_place_expr(ctx, base)?;
            let mut resolved = ctx.subst.substitute(&base_ty);
            // Auto-deref through `&T` / `&mut T` (matches value-position
            // tuple-index behavior).
            while let InferType::Ref { inner, .. } = resolved {
                resolved = ctx.subst.substitute(&inner);
            }
            match resolved {
                InferType::Tuple(elems) => {
                    let n = elems.len();
                    if (*index as usize) >= n {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message: format!(
                                "tuple index {} out of range (length {})",
                                index, n
                            ),
                            span: index_span.copy(),
                        });
                    }
                    Ok(infer_clone(&elems[*index as usize]))
                }
                other => Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "tuple index `.{}` on non-tuple type `{}`",
                        index,
                        infer_to_string(&other)
                    ),
                    span: expr.span.copy(),
                }),
            }
        }
        _ => unreachable!("check_place_inner only dispatches Var/FieldAccess/Deref/TupleIndex"),
    }
}

fn check_deref(ctx: &mut CheckCtx, inner: &Expr, deref_expr: &Expr) -> Result<InferType, Error> {
    let inner_ty = check_expr(ctx, inner)?;
    let resolved = ctx.subst.substitute(&inner_ty);
    match resolved {
        InferType::Ref { inner, .. } => Ok(*inner),
        InferType::RawPtr { inner, .. } => Ok(*inner),
        other => Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot dereference `{}` — only references and raw pointers can be dereferenced",
                infer_to_string(&other)
            ),
            span: deref_expr.span.copy(),
        }),
    }
}

// Casts in our subset: any of {integer, &T, &mut T, *const T, *mut T} can be
// cast to any of {*const T, *mut T}. Integer-class type vars get pinned to
// usize at the cast site (matches Rust's "integers cast to ptr-sized") so the
// underlying ABI is i32. Everything else is a type-level reinterpret.
fn check_cast(
    ctx: &mut CheckCtx,
    inner: &Expr,
    ty: &Type,
    cast_expr: &Expr,
) -> Result<InferType, Error> {
    let target = resolve_type(
        ty,
        ctx.current_module,
        ctx.structs,
        ctx.enums,
        ctx.self_target,
        ctx.type_params,
        &ctx.use_scope,
        ctx.reexports,
        ctx.current_file,
    )?;
    let target_is_ptr = is_raw_ptr(&target);
    let target_is_int = matches!(&target, RType::Int(_));
    if !target_is_ptr && !target_is_int {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "casts are only allowed to raw pointer or integer types, got `{}`",
                rtype_to_string(&target)
            ),
            span: ty.span.copy(),
        });
    }
    let src_ty = check_expr(ctx, inner)?;
    let resolved_src = ctx.subst.substitute(&src_ty);
    let src_ok = if target_is_ptr {
        matches!(
            &resolved_src,
            InferType::Ref { .. }
                | InferType::RawPtr { .. }
                | InferType::Int(_)
                | InferType::Var(_)
        )
    } else {
        // Int target: source must be an integer (or unbound integer var).
        matches!(&resolved_src, InferType::Int(_) | InferType::Var(_))
    };
    if !src_ok {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot cast `{}` to `{}`",
                infer_to_string(&resolved_src),
                rtype_to_string(&target)
            ),
            span: cast_expr.span.copy(),
        });
    }
    if let InferType::Var(v) = &resolved_src {
        if ctx.subst.is_num_lit[*v as usize] {
            // Pin an unresolved integer literal: to `usize` for ptr casts
            // (matches "integers cast to ptr-sized"), to `i32` otherwise.
            let pin_kind = if target_is_ptr {
                IntKind::Usize
            } else {
                IntKind::I32
            };
            ctx.subst.unify(
                &InferType::Var(*v),
                &InferType::Int(pin_kind),
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &cast_expr.span,
                ctx.current_file,
            )?;
        }
    }
    Ok(rtype_to_infer(&target))
}

fn check_call(ctx: &mut CheckCtx, call: &Call, call_expr: &Expr) -> Result<InferType, Error> {
    // Use-table resolution first: an explicit import or matching glob
    // takes precedence over module-relative path lookup, so
    // `use std::dummy::id; id(7)` resolves to `["std","dummy","id"]`
    // rather than `[<current-module>, "id"]`.
    let raw_segs: Vec<String> =
        call.callee.segments.iter().map(|s| s.name.clone()).collect();
    // Try enum-variant construction first: `Path::Variant(args)`. The
    // path's prefix names an enum and the last segment matches a variant.
    if let Some((enum_path, disc)) = lookup_variant_path(
        ctx.enums,
        ctx.reexports,
        &ctx.use_scope,
        ctx.current_module,
        &raw_segs,
    ) {
        return check_variant_call(ctx, call, call_expr, enum_path, disc);
    }
    let raw_full =
        resolve_via_use_scopes(&raw_segs, &ctx.use_scope, |cand| {
            func_path_resolved(ctx.funcs, ctx.reexports, cand).is_some()
        })
        .unwrap_or_else(|| {
            resolve_full_path(ctx.current_module, ctx.self_target, &call.callee.segments)
        });
    // Follow re-exports to the canonical path so the FuncTable lookups
    // below find the entry/template.
    let full = func_path_resolved(ctx.funcs, ctx.reexports, &raw_full).unwrap_or(raw_full);
    // Generic args attach to the last segment of the callee path.
    let last_seg_args = if call.callee.segments.is_empty() {
        Vec::new()
    } else {
        let last = &call.callee.segments[call.callee.segments.len() - 1];
        let mut v: Vec<crate::ast::Type> = Vec::new();
        let mut i = 0;
        while i < last.args.len() {
            v.push(last.args[i].clone());
            i += 1;
        }
        v
    };
    // Try non-generic first.
    if let Some(entry_idx) = funcs_entry_index(ctx.funcs, &full) {
        let entry = &ctx.funcs.entries[entry_idx];
        let is_method = entry.impl_target.is_some();
        let def_mod = fn_defining_module(&entry.path, is_method);
        if !is_visible_from(&def_mod, entry.is_pub, ctx.current_module) {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "function `{}` is private",
                    place_to_string(&entry.path)
                ),
                span: call_expr.span.copy(),
            });
        }
        if !last_seg_args.is_empty() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "`{}` is not a generic function — turbofish is not allowed",
                    segments_to_string(&call.callee.segments)
                ),
                span: call_expr.span.copy(),
            });
        }
        if call.args.len() != entry.param_types.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of arguments to `{}`: expected {}, got {}",
                    segments_to_string(&call.callee.segments),
                    entry.param_types.len(),
                    call.args.len()
                ),
                span: call_expr.span.copy(),
            });
        }
        let mut param_infer: Vec<InferType> = Vec::new();
        let mut k = 0;
        while k < entry.param_types.len() {
            param_infer.push(rtype_to_infer(&entry.param_types[k]));
            k += 1;
        }
        let return_infer: InferType = match &entry.return_type {
            Some(rt) => rtype_to_infer(rt),
            None => InferType::Tuple(Vec::new()),
        };
        ctx.call_resolutions[call_expr.id as usize] = Some(PendingCall::Direct(entry_idx));
        let mut i = 0;
        while i < call.args.len() {
            let arg_ty = check_expr(ctx, &call.args[i])?;
            ctx.subst.unify(
                &arg_ty,
                &param_infer[i],
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &call.args[i].span,
                ctx.current_file,
            )?;
            i += 1;
        }
        return Ok(return_infer);
    }
    // Try a generic template.
    if let Some((template_idx, _)) = template_lookup(ctx.funcs, &full) {
        let tmpl_is_pub = ctx.funcs.templates[template_idx].is_pub;
        let tmpl_path = clone_path(&ctx.funcs.templates[template_idx].path);
        let tmpl_is_method = ctx.funcs.templates[template_idx].impl_target.is_some();
        let def_mod = fn_defining_module(&tmpl_path, tmpl_is_method);
        if !is_visible_from(&def_mod, tmpl_is_pub, ctx.current_module) {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "function `{}` is private",
                    place_to_string(&tmpl_path)
                ),
                span: call_expr.span.copy(),
            });
        }
        // Snapshot the template's data we need (clone vectors so we don't keep
        // a borrow into ctx.funcs across the upcoming ctx.subst mutations).
        let tmpl_type_params: Vec<String> = ctx.funcs.templates[template_idx].type_params.clone();
        let tmpl_param_types: Vec<RType> = {
            let mut v: Vec<RType> = Vec::new();
            let mut k = 0;
            while k < ctx.funcs.templates[template_idx].param_types.len() {
                v.push(rtype_clone(&ctx.funcs.templates[template_idx].param_types[k]));
                k += 1;
            }
            v
        };
        let tmpl_return_type: Option<RType> = ctx.funcs.templates[template_idx]
            .return_type
            .as_ref()
            .map(rtype_clone);
        if !last_seg_args.is_empty() && last_seg_args.len() != tmpl_type_params.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of type arguments to `{}`: expected {}, got {}",
                    segments_to_string(&call.callee.segments),
                    tmpl_type_params.len(),
                    last_seg_args.len()
                ),
                span: call_expr.span.copy(),
            });
        }
        if call.args.len() != tmpl_param_types.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of arguments to `{}`: expected {}, got {}",
                    segments_to_string(&call.callee.segments),
                    tmpl_param_types.len(),
                    call.args.len()
                ),
                span: call_expr.span.copy(),
            });
        }
        let mut env: Vec<(String, InferType)> = Vec::new();
        let mut var_ids: Vec<u32> = Vec::new();
        let mut k = 0;
        while k < tmpl_type_params.len() {
            let v = ctx.subst.fresh_var();
            var_ids.push(v);
            env.push((tmpl_type_params[k].clone(), InferType::Var(v)));
            k += 1;
        }
        // Apply explicit turbofish args by unifying.
        let mut k = 0;
        while k < last_seg_args.len() {
            let user_rt = resolve_type(
                &last_seg_args[k],
                ctx.current_module,
                ctx.structs,
                ctx.enums,
                ctx.self_target,
                ctx.type_params,
                &ctx.use_scope,
                ctx.reexports,
                ctx.current_file,
            )?;
            let user_infer = rtype_to_infer(&user_rt);
            ctx.subst.unify(
                &InferType::Var(var_ids[k]),
                &user_infer,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &last_seg_args[k].span,
                ctx.current_file,
            )?;
            k += 1;
        }
        let mut param_infer: Vec<InferType> = Vec::new();
        let mut k = 0;
        while k < tmpl_param_types.len() {
            param_infer.push(infer_substitute(&rtype_to_infer(&tmpl_param_types[k]), &env));
            k += 1;
        }
        let return_infer: InferType = match &tmpl_return_type {
            Some(rt) => infer_substitute(&rtype_to_infer(rt), &env),
            None => InferType::Tuple(Vec::new()),
        };
        ctx.call_resolutions[call_expr.id as usize] = Some(PendingCall::Generic {
            template_idx,
            type_var_ids: var_ids,
        });
        let mut i = 0;
        while i < call.args.len() {
            let arg_ty = check_expr(ctx, &call.args[i])?;
            ctx.subst.unify(
                &arg_ty,
                &param_infer[i],
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &call.args[i].span,
                ctx.current_file,
            )?;
            i += 1;
        }
        return Ok(return_infer);
    }
    Err(Error {
        file: ctx.current_file.to_string(),
        message: format!(
            "unresolved function: {}",
            segments_to_string(&call.callee.segments)
        ),
        span: call.callee.span.copy(),
    })
}

// Struct-shaped variant: `E::Variant { f: e, g: e }`. Mirrors
// `check_variant_call` but matches the named-field shape.
fn check_variant_struct_lit(
    ctx: &mut CheckCtx,
    lit: &StructLit,
    lit_expr: &Expr,
    enum_path: Vec<String>,
    disc: usize,
) -> Result<InferType, Error> {
    let entry = enum_lookup(ctx.enums, &enum_path).expect("variant lookup returned a real enum");
    if !is_visible_from(
        &type_defining_module(&entry.path),
        entry.is_pub,
        ctx.current_module,
    ) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("enum `{}` is private", place_to_string(&entry.path)),
            span: lit.path.span.copy(),
        });
    }
    let variant = &entry.variants[disc];
    let field_defs: Vec<RTypedField> = match &variant.payload {
        VariantPayloadResolved::Struct(fields) => {
            let mut out: Vec<RTypedField> = Vec::new();
            let mut k = 0;
            while k < fields.len() {
                out.push(RTypedField {
                    name: fields[k].name.clone(),
                    name_span: fields[k].name_span.copy(),
                    ty: rtype_clone(&fields[k].ty),
                    is_pub: fields[k].is_pub,
                });
                k += 1;
            }
            out
        }
        VariantPayloadResolved::Tuple(_) => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "variant `{}::{}` is a tuple-shaped variant; use `{}::{}( … )`",
                    place_to_string(&entry.path),
                    variant.name,
                    place_to_string(&entry.path),
                    variant.name
                ),
                span: lit.path.span.copy(),
            });
        }
        VariantPayloadResolved::Unit => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "variant `{}::{}` is a unit variant; use `{}::{}` (no braces)",
                    place_to_string(&entry.path),
                    variant.name,
                    place_to_string(&entry.path),
                    variant.name
                ),
                span: lit.path.span.copy(),
            });
        }
    };
    // Type args from the enum's prefix segment (e.g. `Option::<u32>::Some { … }`).
    let last = &lit.path.segments[lit.path.segments.len() - 1];
    let mut explicit_type_args: Vec<crate::ast::Type> = last.args.clone();
    if explicit_type_args.is_empty() && lit.path.segments.len() >= 2 {
        let prev = &lit.path.segments[lit.path.segments.len() - 2];
        explicit_type_args = prev.args.clone();
    }
    if !explicit_type_args.is_empty() && explicit_type_args.len() != entry.type_params.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of type arguments for `{}`: expected {}, got {}",
                place_to_string(&entry.path),
                entry.type_params.len(),
                explicit_type_args.len()
            ),
            span: lit.path.span.copy(),
        });
    }
    let mut type_var_ids: Vec<u32> = Vec::with_capacity(entry.type_params.len());
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut k = 0;
    while k < entry.type_params.len() {
        let v = ctx.subst.fresh_var();
        type_var_ids.push(v);
        env.push((entry.type_params[k].clone(), InferType::Var(v)));
        k += 1;
    }
    if !explicit_type_args.is_empty() {
        let mut k = 0;
        while k < explicit_type_args.len() {
            let rt = resolve_type(
                &explicit_type_args[k],
                ctx.current_module,
                ctx.structs,
                ctx.enums,
                ctx.self_target,
                ctx.type_params,
                &ctx.use_scope,
                ctx.reexports,
                ctx.current_file,
            )?;
            ctx.subst.unify(
                &InferType::Var(type_var_ids[k]),
                &rtype_to_infer(&rt),
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &lit.path.span,
                ctx.current_file,
            )?;
            k += 1;
        }
    }
    // Validate field set: every declared field present, no extras, no
    // duplicates. Check each initializer's type against the substituted
    // declared type.
    let enum_path_clone = clone_path(&entry.path);
    let variant_name = variant.name.clone();
    let mut seen: Vec<bool> = vec![false; field_defs.len()];
    let mut i = 0;
    while i < lit.fields.len() {
        let init = &lit.fields[i];
        let mut found: Option<usize> = None;
        let mut k = 0;
        while k < field_defs.len() {
            if field_defs[k].name == init.name {
                found = Some(k);
                break;
            }
            k += 1;
        }
        let idx = match found {
            Some(idx) => idx,
            None => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "unknown field `{}` on variant `{}::{}`",
                        init.name,
                        place_to_string(&enum_path_clone),
                        variant_name
                    ),
                    span: init.name_span.copy(),
                });
            }
        };
        if seen[idx] {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("duplicate field `{}` in variant literal", init.name),
                span: init.name_span.copy(),
            });
        }
        seen[idx] = true;
        let value_ty = check_expr(ctx, &init.value)?;
        let expected = infer_substitute(&rtype_to_infer(&field_defs[idx].ty), &env);
        ctx.subst.unify(
            &value_ty,
            &expected,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &init.value.span,
            ctx.current_file,
        )?;
        i += 1;
    }
    let mut k = 0;
    while k < field_defs.len() {
        if !seen[k] {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "missing field `{}` in variant `{}::{}`",
                    field_defs[k].name,
                    place_to_string(&enum_path_clone),
                    variant_name
                ),
                span: lit.path.span.copy(),
            });
        }
        k += 1;
    }
    let disc_u32 = disc as u32;
    ctx.call_resolutions[lit_expr.id as usize] = Some(PendingCall::Variant {
        enum_path: clone_path(&enum_path_clone),
        disc: disc_u32,
        type_var_ids: type_var_ids.clone(),
    });
    let mut type_args_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < type_var_ids.len() {
        type_args_infer.push(InferType::Var(type_var_ids[k]));
        k += 1;
    }
    Ok(InferType::Enum {
        path: enum_path_clone,
        type_args: type_args_infer,
        lifetime_args: Vec::new(),
    })
}

fn funcs_entry_index(funcs: &FuncTable, path: &Vec<String>) -> Option<usize> {
    let mut i = 0;
    while i < funcs.entries.len() {
        if path_eq(&funcs.entries[i].path, path) {
            return Some(i);
        }
        i += 1;
    }
    None
}

// `E::Variant(args)` — enum variant construction with positional payload
// (or no payload, in which case args must be empty). Resolves the variant,
// allocates fresh inference vars for the enum's type-params, type-checks
// the args against the variant's payload types substituted with those
// vars, and returns `InferType::Enum`. Records a `PendingCall::Variant`
// at this Call's NodeId so codegen can lower construction.
fn check_variant_call(
    ctx: &mut CheckCtx,
    call: &Call,
    call_expr: &Expr,
    enum_path: Vec<String>,
    disc: usize,
) -> Result<InferType, Error> {
    let entry = enum_lookup(ctx.enums, &enum_path).expect("variant lookup returned a real enum");
    if !is_visible_from(
        &type_defining_module(&entry.path),
        entry.is_pub,
        ctx.current_module,
    ) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("enum `{}` is private", place_to_string(&entry.path)),
            span: call_expr.span.copy(),
        });
    }
    let variant = &entry.variants[disc];
    // Resolve the type-args slot: turbofish on the last seg goes to the
    // variant, but Rust convention is `E::<T>::Variant(args)` so type-
    // args attach to the enum-prefix segment. We accept either: pull
    // type args off whichever segment carries them.
    let last = &call.callee.segments[call.callee.segments.len() - 1];
    let mut explicit_type_args: Vec<crate::ast::Type> = last.args.clone();
    if explicit_type_args.is_empty() && call.callee.segments.len() >= 2 {
        let prev = &call.callee.segments[call.callee.segments.len() - 2];
        explicit_type_args = prev.args.clone();
    }
    if !explicit_type_args.is_empty() && explicit_type_args.len() != entry.type_params.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of type arguments for `{}`: expected {}, got {}",
                place_to_string(&entry.path),
                entry.type_params.len(),
                explicit_type_args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    // Allocate fresh inference vars for each enum type-param. If the user
    // provided turbofish args, immediately bind each var to the explicit
    // type. Otherwise inference will close them via arg-type unification.
    let mut type_var_ids: Vec<u32> = Vec::with_capacity(entry.type_params.len());
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut k = 0;
    while k < entry.type_params.len() {
        let v = ctx.subst.fresh_var();
        type_var_ids.push(v);
        env.push((entry.type_params[k].clone(), InferType::Var(v)));
        k += 1;
    }
    if !explicit_type_args.is_empty() {
        let mut k = 0;
        while k < explicit_type_args.len() {
            let rt = resolve_type(
                &explicit_type_args[k],
                ctx.current_module,
                ctx.structs,
                ctx.enums,
                ctx.self_target,
                ctx.type_params,
                &ctx.use_scope,
                ctx.reexports,
                ctx.current_file,
            )?;
            let infer = rtype_to_infer(&rt);
            let var_infer = InferType::Var(type_var_ids[k]);
            ctx.subst.unify(
                &var_infer,
                &infer,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &call_expr.span,
                ctx.current_file,
            )?;
            k += 1;
        }
    }
    // Validate payload shape and check arg types.
    let payload_types: Vec<RType> = match &variant.payload {
        VariantPayloadResolved::Unit => Vec::new(),
        VariantPayloadResolved::Tuple(types) => rtype_vec_clone(types),
        VariantPayloadResolved::Struct(_) => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "variant `{}::{}` is a struct-shaped variant; use `{}::{} {{ … }}`",
                    place_to_string(&entry.path),
                    variant.name,
                    place_to_string(&entry.path),
                    variant.name
                ),
                span: call_expr.span.copy(),
            });
        }
    };
    if call.args.len() != payload_types.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}::{}`: expected {}, got {}",
                place_to_string(&entry.path),
                variant.name,
                payload_types.len(),
                call.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let disc_u32 = disc as u32;
    ctx.call_resolutions[call_expr.id as usize] = Some(PendingCall::Variant {
        enum_path: clone_path(&entry.path),
        disc: disc_u32,
        type_var_ids: type_var_ids.clone(),
    });
    let mut i = 0;
    while i < payload_types.len() {
        let arg_ty = check_expr(ctx, &call.args[i])?;
        let expected = infer_substitute(&rtype_to_infer(&payload_types[i]), &env);
        ctx.subst.unify(
            &arg_ty,
            &expected,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &call.args[i].span,
            ctx.current_file,
        )?;
        i += 1;
    }
    // Build the result type: the enum, instantiated with the
    // inference vars (which the literal/arg unification above has
    // pinned where possible).
    let mut type_args_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < type_var_ids.len() {
        type_args_infer.push(InferType::Var(type_var_ids[k]));
        k += 1;
    }
    Ok(InferType::Enum {
        path: clone_path(&entry.path),
        type_args: type_args_infer,
        lifetime_args: Vec::new(),
    })
}

fn check_struct_lit(
    ctx: &mut CheckCtx,
    lit: &StructLit,
    lit_expr: &Expr,
) -> Result<InferType, Error> {
    let raw_segs: Vec<String> = lit.path.segments.iter().map(|s| s.name.clone()).collect();
    // Try enum struct-variant construction first. If the path's
    // prefix names an enum and the last segment is a struct-shaped
    // variant, route to the variant-construction path.
    if let Some((enum_path, disc)) = lookup_variant_path(
        ctx.enums,
        ctx.reexports,
        &ctx.use_scope,
        ctx.current_module,
        &raw_segs,
    ) {
        return check_variant_struct_lit(ctx, lit, lit_expr, enum_path, disc);
    }
    let raw_full =
        resolve_via_use_scopes(&raw_segs, &ctx.use_scope, |cand| {
            struct_lookup_resolved(ctx.structs, ctx.reexports, cand).is_some()
        })
        .unwrap_or_else(|| {
            resolve_full_path(ctx.current_module, ctx.self_target, &lit.path.segments)
        });
    // Follow re-exports to the struct's canonical path.
    let full = struct_lookup_resolved(ctx.structs, ctx.reexports, &raw_full)
        .map(|e| clone_path(&e.path))
        .unwrap_or(raw_full);

    let entry = match struct_lookup(ctx.structs, &full) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unknown struct: {}",
                    segments_to_string(&lit.path.segments)
                ),
                span: lit.path.span.copy(),
            });
        }
    };
    if !is_visible_from(&type_defining_module(&entry.path), entry.is_pub, ctx.current_module) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("struct `{}` is private", place_to_string(&entry.path)),
            span: lit.path.span.copy(),
        });
    }
    let struct_type_params: Vec<String> = entry.type_params.clone();
    let mut def_field_names: Vec<String> = Vec::new();
    let mut def_field_types: Vec<RType> = Vec::new();
    let mut def_field_pubs: Vec<bool> = Vec::new();
    let mut k = 0;
    while k < entry.fields.len() {
        def_field_names.push(entry.fields[k].name.clone());
        def_field_types.push(rtype_clone(&entry.fields[k].ty));
        def_field_pubs.push(entry.fields[k].is_pub);
        k += 1;
    }
    // Field-level visibility: constructing a struct from outside its
    // defining module requires every field to be `pub`. Inside the
    // module, all fields are reachable regardless.
    let struct_def_module = type_defining_module(&entry.path);
    let inside_def_module: bool =
        is_visible_from(&struct_def_module, false, ctx.current_module);
    // Allocate fresh type-arg vars for this struct's params. If the path's
    // last segment carried turbofish args, unify them.
    let last_seg = &lit.path.segments[lit.path.segments.len() - 1];
    if !last_seg.args.is_empty() && last_seg.args.len() != struct_type_params.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of type arguments for `{}`: expected {}, got {}",
                place_to_string(&full),
                struct_type_params.len(),
                last_seg.args.len()
            ),
            span: lit.path.span.copy(),
        });
    }
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut type_arg_infers: Vec<InferType> = Vec::new();
    let mut i = 0;
    while i < struct_type_params.len() {
        let v = ctx.subst.fresh_var();
        type_arg_infers.push(InferType::Var(v));
        env.push((struct_type_params[i].clone(), InferType::Var(v)));
        i += 1;
    }
    let mut k = 0;
    while k < last_seg.args.len() {
        let user_rt = resolve_type(
            &last_seg.args[k],
            ctx.current_module,
            ctx.structs,
            ctx.enums,
            ctx.self_target,
            ctx.type_params,
            &ctx.use_scope,
            ctx.reexports,
            ctx.current_file,
        )?;
        let user_infer = rtype_to_infer(&user_rt);
        ctx.subst.unify(
            &type_arg_infers[k],
            &user_infer,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &last_seg.args[k].span,
            ctx.current_file,
        )?;
        k += 1;
    }
    // (The wrapping `check_expr` will store our return value at this Expr's
    // NodeId — that gives codegen the concrete type_args for layout.)

    // Validate field shape.
    let mut i = 0;
    while i < lit.fields.len() {
        let mut found_idx: Option<usize> = None;
        let mut j = 0;
        while j < def_field_names.len() {
            if lit.fields[i].name == def_field_names[j] {
                found_idx = Some(j);
                break;
            }
            j += 1;
        }
        let found_idx = match found_idx {
            Some(j) => j,
            None => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "struct `{}` has no field `{}`",
                        segments_to_string(&lit.path.segments),
                        lit.fields[i].name
                    ),
                    span: lit.fields[i].name_span.copy(),
                });
            }
        };
        if !inside_def_module && !def_field_pubs[found_idx] {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "field `{}` of `{}` is private",
                    lit.fields[i].name,
                    place_to_string(&full)
                ),
                span: lit.fields[i].name_span.copy(),
            });
        }
        let mut k = i + 1;
        while k < lit.fields.len() {
            if lit.fields[k].name == lit.fields[i].name {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("field `{}` is initialized twice", lit.fields[i].name),
                    span: lit.fields[k].name_span.copy(),
                });
            }
            k += 1;
        }
        i += 1;
    }
    let mut i = 0;
    while i < def_field_names.len() {
        let mut present = false;
        let mut j = 0;
        while j < lit.fields.len() {
            if lit.fields[j].name == def_field_names[i] {
                present = true;
                break;
            }
            j += 1;
        }
        if !present {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("missing field `{}`", def_field_names[i]),
                span: lit_expr.span.copy(),
            });
        }
        i += 1;
    }

    // Type-check inits in source order. Each declared field type is
    // substituted via the struct's type-arg env so Param("T") in field types
    // unifies with whatever the type-arg var resolves to.
    let mut i = 0;
    while i < lit.fields.len() {
        let init = &lit.fields[i];
        let init_ty = check_expr(ctx, &init.value)?;
        let mut k = 0;
        while k < def_field_names.len() {
            if def_field_names[k] == init.name {
                let expected_raw = rtype_to_infer(&def_field_types[k]);
                let expected = infer_substitute(&expected_raw, &env);
                ctx.subst.unify(
                    &init_ty,
                    &expected,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    &init.value.span,
                    ctx.current_file,
                )?;
                break;
            }
            k += 1;
        }
        i += 1;
    }

    // Struct literals allocate fresh `Inferred(0)` placeholders for their
    // lifetime args — Phase D doesn't unify struct lifetimes (carry-only),
    // and borrowck reads field borrows directly from the holder's per-slot
    // data rather than from these placeholders.
    let mut lit_lifetime_args: Vec<LifetimeRepr> = Vec::new();
    let mut i = 0;
    while i < entry.lifetime_params.len() {
        lit_lifetime_args.push(LifetimeRepr::Inferred(0));
        i += 1;
    }
    Ok(InferType::Struct {
        path: full,
        type_args: type_arg_infers,
        lifetime_args: lit_lifetime_args,
    })
}

fn check_field_access(
    ctx: &mut CheckCtx,
    fa: &FieldAccess,
    _fa_expr: &Expr,
) -> Result<InferType, Error> {
    // Field access through a deref expression — `(*p).field` — applies the
    // same "Copy fields only" rule as auto-deref `r.field` does. Detect it
    // syntactically before walking the base.
    let through_explicit_deref = matches!(&fa.base.kind, ExprKind::Deref(_));
    let base_ty = check_expr(ctx, &fa.base)?;
    let resolved = ctx.subst.substitute(&base_ty);
    let (struct_path, struct_type_args, through_ref) = match resolved {
        InferType::Struct { path, type_args, .. } => (path, type_args, through_explicit_deref),
        InferType::Ref { inner, .. } => match *inner {
            InferType::Struct { path, type_args, .. } => (path, type_args, true),
            _ => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "field access on non-struct value".to_string(),
                    span: fa.base.span.copy(),
                });
            }
        },
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "field access on non-struct value".to_string(),
                span: fa.base.span.copy(),
            });
        }
    };
    let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
    let mut i = 0;
    while i < entry.fields.len() {
        if entry.fields[i].name == fa.field {
            // Field-level visibility: a non-pub field is only readable
            // from inside the struct's defining module (or descendants).
            if !field_visible_from(&struct_path, entry.fields[i].is_pub, ctx.current_module) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "field `{}` of `{}` is private",
                        fa.field,
                        place_to_string(&struct_path)
                    ),
                    span: fa.field_span.copy(),
                });
            }
            // Substitute the field's declared type with the struct's type args
            // (e.g., `pair.first` where pair: Pair<u32, u64> and field declared
            // as T → resolves to u32).
            let env = build_infer_env(&entry.type_params, &struct_type_args);
            let field_ty_raw = rtype_clone(&entry.fields[i].ty);
            let field_infer_raw = rtype_to_infer(&field_ty_raw);
            let field_infer = infer_substitute(&field_infer_raw, &env);
            // Copy check: a non-Copy field accessed through a ref is a move
            // out of borrow. Place borrows (`&...`) walk through
            // `check_place_expr` and skip this branch entirely; only
            // value-position field access reaches here.
            if through_ref
                && !is_copy_with_bounds(
                    &field_ty_raw,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                )
            {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of borrow: field `{}` of `{}` has non-Copy type `{}`",
                        fa.field,
                        place_to_string(&struct_path),
                        rtype_to_string(&field_ty_raw)
                    ),
                    span: fa.field_span.copy(),
                });
            }
            return Ok(field_infer);
        }
        i += 1;
    }
    Err(Error {
        file: ctx.current_file.to_string(),
        message: format!(
            "no field `{}` on `{}`",
            fa.field,
            place_to_string(&struct_path)
        ),
        span: fa.field_span.copy(),
    })
}
