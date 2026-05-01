use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, Stmt, StructLit, Type,
};
use crate::span::{Error, Span};

mod types;
pub use types::{
    IntKind, LifetimeRepr, RType, byte_size_of, copy_trait_path, drop_trait_path, flatten_rtype,
    int_kind_name, is_copy, is_copy_with_bounds, is_drop, is_raw_ptr, is_ref_mutable,
    outer_lifetime, rtype_eq, rtype_to_string, substitute_rtype,
};
use types::{int_kind_from_name, int_kind_max, struct_env};

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
pub(crate) fn infer_to_rtype_for_check(t: &InferType) -> RType {
    match t {
        InferType::Var(_) => RType::Int(IntKind::I32),
        InferType::Int(k) => RType::Int(*k),
        InferType::Struct { path, type_args, lifetime_args } => {
            let mut args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                args.push(infer_to_rtype_for_check(&type_args[i]));
                i += 1;
            }
            RType::Struct {
                path: path.clone(),
                type_args: args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        InferType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(infer_to_rtype_for_check(inner)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
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
                path: path.clone(),
                type_args: args,
                lifetime_args: lifetime_args.clone(),
            }
        }
    }
}


// Returns indices of every param whose outermost ref lifetime equals
// `target`. Empty if no param matches. Phase D returns multiple matches:
// when `'a` ties multiple ref params to the return, all those args'
// borrows propagate into the result (the "combined borrow sets" rule).
mod lifetimes;
pub use lifetimes::find_lifetime_source;
use lifetimes::{
    find_elision_source, freshen_inferred_lifetimes, require_no_inferred_lifetimes,
    validate_named_lifetimes,
};

mod use_scope;
pub use use_scope::{
    ReExportTable, UseEntry, build_reexport_table, field_visible_from, flatten_use_tree,
    fn_defining_module, func_path_resolved, is_visible_from, module_use_entries,
    resolve_via_reexports, resolve_via_use_scopes, struct_lookup_resolved,
    trait_lookup_resolved, type_defining_module,
};

mod path_resolve;
pub use path_resolve::{
    enum_lookup_resolved, lookup_variant_path, place_to_string, resolve_full_path, resolve_type,
    segments_to_string,
};

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


// ----- InferType -----

#[derive(Clone)]
pub(crate) enum InferType {
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

// Build a name → InferType env from a generic struct/template's type-param
// names paired with the call site's type arguments. Used to substitute Param
// in field types or method signatures.
pub(crate) fn build_infer_env(type_params: &Vec<String>, type_args: &Vec<InferType>) -> Vec<(String, InferType)> {
    let mut env: Vec<(String, InferType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), type_args[i].clone()));
        i += 1;
    }
    env
}

pub(crate) fn rtype_to_infer(rt: &RType) -> InferType {
    match rt {
        RType::Int(k) => InferType::Int(*k),
        RType::Struct { path, type_args, lifetime_args } => {
            let mut infer_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                infer_args.push(rtype_to_infer(&type_args[i]));
                i += 1;
            }
            InferType::Struct {
                path: path.clone(),
                type_args: infer_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        RType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
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
                path: path.clone(),
                type_args: infer_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
    }
}

// Substitute type parameters in an InferType using a name → InferType env.
// Used at generic call sites to map the callee's `Param("T")` slots to fresh
// inference vars allocated for the call.
pub(crate) fn infer_substitute(t: &InferType, env: &Vec<(String, InferType)>) -> InferType {
    match t {
        InferType::Var(v) => InferType::Var(*v),
        InferType::Int(k) => InferType::Int(*k),
        InferType::Struct { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(infer_substitute(&type_args[i], env));
                i += 1;
            }
            InferType::Struct {
                path: path.clone(),
                type_args: subst_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        InferType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(infer_substitute(inner, env)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_substitute(inner, env)),
            mutable: *mutable,
        },
        InferType::Param(name) => {
            let mut i = 0;
            while i < env.len() {
                if env[i].0 == *name {
                    return env[i].1.clone();
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
                path: path.clone(),
                type_args: subst_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
    }
}

pub(crate) fn infer_to_string(t: &InferType) -> String {
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

pub(crate) struct Subst {
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
    pub(crate) fn fresh_int(&mut self) -> u32 {
        let id = self.bindings.len() as u32;
        self.bindings.push(None);
        self.is_num_lit.push(true);
        id
    }

    pub(crate) fn fresh_var(&mut self) -> u32 {
        let id = self.bindings.len() as u32;
        self.bindings.push(None);
        self.is_num_lit.push(false);
        id
    }

    pub(crate) fn substitute(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Var(v) => match &self.bindings[*v as usize] {
                Some(t) => self.substitute(t),
                None => InferType::Var(*v),
            },
            InferType::Int(k) => InferType::Int(*k),
            InferType::Struct { path, type_args, lifetime_args } => {
                let mut subst_args: Vec<InferType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    subst_args.push(self.substitute(&type_args[i]));
                    i += 1;
                }
                InferType::Struct {
                    path: path.clone(),
                    type_args: subst_args,
                    lifetime_args: lifetime_args.clone(),
                }
            }
            InferType::Ref { inner, mutable, lifetime } => InferType::Ref {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
                lifetime: lifetime.clone(),
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
                    path: path.clone(),
                    type_args: subst_args,
                    lifetime_args: lifetime_args.clone(),
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


    pub(crate) fn unify(
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
                if ka == kb {
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
                if pa != pb {
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
                if pa != pb {
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

pub(crate) struct LitConstraint {
    var: u32,
    value: u64,
    span: Span,
}

pub(crate) struct LocalEntry {
    name: String,
    ty: InferType,
    mutable: bool,
}

pub(crate) struct CheckCtx<'a> {
    pub(crate) locals: Vec<LocalEntry>,
    // Per-NodeId InferType (sized to func.node_count). After body check,
    // each entry is finalized into the FnSymbol/GenericTemplate's expr_types.
    pub(crate) expr_infer_types: Vec<Option<InferType>>,
    pub(crate) lit_constraints: Vec<LitConstraint>,
    // Pending per-MethodCall and per-Call resolutions, indexed by Expr.id.
    pub(crate) method_resolutions: Vec<Option<PendingMethodCall>>,
    pub(crate) call_resolutions: Vec<Option<PendingCall>>,
    pub(crate) subst: Subst,
    pub(crate) current_module: &'a Vec<String>,
    pub(crate) current_file: &'a str,
    pub(crate) structs: &'a StructTable,
    pub(crate) enums: &'a EnumTable,
    pub(crate) traits: &'a TraitTable,
    pub(crate) funcs: &'a FuncTable,
    pub(crate) self_target: Option<&'a RType>,
    pub(crate) type_params: &'a Vec<String>,
    pub(crate) reexports: &'a ReExportTable,
    // Active use entries, ordered with the outermost (module-level)
    // entries first and innermost-block entries appended at the end.
    // Path resolution iterates this in reverse so the innermost scope
    // shadows outer ones. Block walks save `use_scope.len()` before
    // entering and truncate back on exit.
    pub(crate) use_scope: Vec<UseEntry>,
    // Per-type-param trait bounds (resolved trait paths) for the
    // currently-checked function. Same shape as
    // `GenericTemplate.type_param_bounds` — `[i]` lists the bound traits
    // on `type_params[i]`. Empty for non-generic functions.
    pub(crate) type_param_bounds: &'a Vec<Vec<Vec<String>>>,
    // Stack of enclosing loop labels (innermost-last). Each entry is
    // the loop's optional label; `break`/`continue` validate that any
    // referenced label is in this stack.
    pub(crate) loop_labels: Vec<Option<String>>,
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
                let mut method_prefix = path.clone();
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
        let mut p = path_prefix.clone();
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
            loop_labels: Vec::new(),
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
                        trait_path: td.trait_path.clone(),
                        method_name: td.method_name.clone(),
                        recv_type: subst.finalize(&td.recv_type_infer),
                    }),
                    None => None,
                };
                method_resolutions_final.push(Some(MethodResolution {
                    callee_idx: p.callee_idx,
                    callee_path: p.callee_path.clone(),
                    recv_adjust: p.recv_adjust,
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
                    enum_path: enum_path.clone(),
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
    let mut full = path_prefix.clone();
    full.push(func.name.clone());
    let mut entry_idx: Option<usize> = None;
    let mut e = 0;
    while e < funcs.entries.len() {
        if funcs.entries[e].path == full {
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
            if funcs.templates[t].path == full {
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

// Per-call recording during body check; resolved at end-of-fn into `CallResolution`.
pub(crate) enum PendingCall {
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
pub(crate) struct PendingMethodCall {
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
        Some(rt) => rt.clone(),
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
    ctx.expr_infer_types[let_stmt.value.id as usize] = Some(final_ty.clone());
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
            InferType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
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
    _enums: &EnumTable,
    file: &str,
    span: &Span,
) -> Result<InferType, Error> {
    let mut current = start.clone();
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
                InferType::Tuple(es) => es.clone(),
                InferType::Ref { inner, .. } => match inner.as_ref() {
                    InferType::Tuple(es) => es.clone(),
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
            current = elems[idx].clone();
            i += 1;
            continue;
        }
        let (struct_path, type_args) = match &current {
            InferType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
            InferType::Ref { inner, .. } => match inner.as_ref() {
                InferType::Struct { path, type_args, .. } => {
                    (path.clone(), type_args.clone())
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

pub(crate) fn check_expr(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
    let ty = check_expr_inner(ctx, expr)?;
    // Record the resolved InferType under this Expr's NodeId. Finalized to
    // RType at end-of-fn into FnSymbol/Template.expr_types.
    ctx.expr_infer_types[expr.id as usize] = Some(ty.clone());
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
                    return Ok(ctx.locals[i].ty.clone());
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
        ExprKind::Builtin { name, name_span, type_args, args } => {
            check_builtin(ctx, name, name_span, type_args, args, expr)
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
                    Ok(elems[*index as usize].clone())
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
        ExprKind::While(w) => check_while_expr(ctx, w, expr),
        ExprKind::Break { label, label_span } => {
            check_loop_label(ctx, label, label_span, &expr.span)?;
            // `break` evaluates to `()` in pocket-rust (no `!` type
            // yet); the surrounding context typically discards it.
            Ok(InferType::Tuple(Vec::new()))
        }
        ExprKind::Continue { label, label_span } => {
            check_loop_label(ctx, label, label_span, &expr.span)?;
            Ok(InferType::Tuple(Vec::new()))
        }
    }
}

// `while cond { body }`. Cond must be bool; body's tail must be ().
// The expression itself has type `()`.
fn check_while_expr(
    ctx: &mut CheckCtx,
    w: &crate::ast::WhileExpr,
    expr: &Expr,
) -> Result<InferType, Error> {
    // Disallow duplicate labels in nested scopes (matches Rust).
    if let Some(name) = &w.label {
        let mut i = ctx.loop_labels.len();
        while i > 0 {
            i -= 1;
            if ctx.loop_labels[i].as_deref() == Some(name.as_str()) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("duplicate loop label `'{}`", name),
                    span: w.label_span.as_ref().map(|s| s.copy()).unwrap_or_else(|| expr.span.copy()),
                });
            }
            i -= 0; // dummy to silence overflow on i==0 path
        }
    }
    let cond_ty = check_expr(ctx, &w.cond)?;
    ctx.subst.unify(
        &cond_ty,
        &InferType::Bool,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &w.cond.span,
        ctx.current_file,
    )?;
    ctx.loop_labels.push(w.label.clone());
    let unit = InferType::Tuple(Vec::new());
    let body_ty = check_block_inner(ctx, w.body.as_ref())?;
    ctx.subst.unify(
        &body_ty,
        &unit,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &w.body.span,
        ctx.current_file,
    )?;
    ctx.loop_labels.pop();
    let _ = expr;
    Ok(InferType::Tuple(Vec::new()))
}

// Validate that a `break`/`continue` is inside a loop, and that any
// named label refers to an active loop in the stack.
fn check_loop_label(
    ctx: &CheckCtx,
    label: &Option<String>,
    label_span: &Option<Span>,
    expr_span: &Span,
) -> Result<(), Error> {
    if ctx.loop_labels.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "`break`/`continue` outside of a loop".to_string(),
            span: expr_span.copy(),
        });
    }
    if let Some(name) = label {
        let mut found = false;
        let mut i = 0;
        while i < ctx.loop_labels.len() {
            if ctx.loop_labels[i].as_deref() == Some(name.as_str()) {
                found = true;
                break;
            }
            i += 1;
        }
        if !found {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown loop label `'{}`", name),
                span: label_span.as_ref().map(|s| s.copy()).unwrap_or_else(|| expr_span.copy()),
            });
        }
    }
    Ok(())
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
                ty: bindings[k].1.clone(),
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
                let prev_clone = prev.clone();
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
            ty: bindings[k].1.clone(),
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


// Builtin intrinsic check. The name encodes (type, op) — e.g.
// `u32_add`, `i64_eq`, `bool_and`, `bool_not` — or names a typed
// intrinsic like `alloc`, `free`, `cast`. Looks up the signature,
// verifies arg arity + types, returns the result type.
//
// Operation kinds:
//   - Arithmetic on int types (add, sub, mul, div, rem): (T, T) -> T.
//   - Comparison on int types (eq, ne, lt, le, gt, ge): (T, T) -> bool.
//   - Bool: and/or (bool, bool) -> bool; not (bool) -> bool;
//     eq/ne (bool, bool) -> bool.
//   - `alloc(n: usize) -> *mut u8`: bump-allocate `n` bytes from the
//     heap; never fails (out-of-memory traps in the wasm host).
//   - `free(p: *mut u8)`: no-op stub today (heap is bump-only); takes
//     and discards a `*mut u8`. Provided as the future hook point for a
//     real allocator.
//   - `cast::<A, B>(p: *const B) -> *const A` and the analogous `*mut B
//     -> *mut A`: changes the pointee type only (mutability is preserved
//     based on the actual arg). Turbofish args A and B are mandatory;
//     type inference is not used. The operation is a no-op at runtime
//     (raw pointers are i32 addresses).
fn check_builtin(
    ctx: &mut CheckCtx,
    name: &str,
    name_span: &Span,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    match name {
        "alloc" => return check_builtin_alloc(ctx, type_args, args, expr),
        "free" => return check_builtin_free(ctx, type_args, args, expr),
        "cast" => return check_builtin_cast(ctx, type_args, args, expr),
        _ => {}
    }
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤{}` does not take type arguments", name),
            span: name_span.copy(),
        });
    }
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

fn check_builtin_alloc(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "builtin `¤alloc` does not take type arguments".to_string(),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤alloc` takes 1 argument, got {}", args.len()),
            span: expr.span.copy(),
        });
    }
    let arg_ty = check_expr(ctx, &args[0])?;
    let expected = rtype_to_infer(&RType::Int(IntKind::Usize));
    ctx.subst.unify(
        &arg_ty,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    Ok(rtype_to_infer(&RType::RawPtr {
        inner: Box::new(RType::Int(IntKind::U8)),
        mutable: true,
    }))
}

fn check_builtin_free(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "builtin `¤free` does not take type arguments".to_string(),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤free` takes 1 argument, got {}", args.len()),
            span: expr.span.copy(),
        });
    }
    let arg_ty = check_expr(ctx, &args[0])?;
    let expected = rtype_to_infer(&RType::RawPtr {
        inner: Box::new(RType::Int(IntKind::U8)),
        mutable: true,
    });
    ctx.subst.unify(
        &arg_ty,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    Ok(InferType::Tuple(Vec::new()))
}

fn check_builtin_cast(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if type_args.len() != 2 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤cast` takes 2 type arguments (`A` and `B`), got {}",
                type_args.len()
            ),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤cast` takes 1 argument, got {}", args.len()),
            span: expr.span.copy(),
        });
    }
    let new_pointee = resolve_type(
        &type_args[0],
        ctx.current_module,
        ctx.structs,
        ctx.enums,
        ctx.self_target,
        ctx.type_params,
        &ctx.use_scope,
        ctx.reexports,
        ctx.current_file,
    )?;
    let old_pointee = resolve_type(
        &type_args[1],
        ctx.current_module,
        ctx.structs,
        ctx.enums,
        ctx.self_target,
        ctx.type_params,
        &ctx.use_scope,
        ctx.reexports,
        ctx.current_file,
    )?;
    let arg_ty = check_expr(ctx, &args[0])?;
    let resolved = ctx.subst.substitute(&arg_ty);
    let mutable = match &resolved {
        InferType::RawPtr { mutable, .. } => *mutable,
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "builtin `¤cast` argument must be a raw pointer, got `{}`",
                    infer_to_string(&resolved)
                ),
                span: args[0].span.copy(),
            });
        }
    };
    let expected_arg = rtype_to_infer(&RType::RawPtr {
        inner: Box::new(old_pointee),
        mutable,
    });
    ctx.subst.unify(
        &arg_ty,
        &expected_arg,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    Ok(rtype_to_infer(&RType::RawPtr {
        inner: Box::new(new_pointee),
        mutable,
    }))
}

mod builtins;
pub use builtins::builtin_signature;

mod patterns;
use patterns::{check_match_exhaustive, check_pattern};

mod methods;
use methods::check_method_call;

mod tables;
pub use tables::{
    CallResolution, EnumEntry, EnumTable, EnumVariantEntry, FnSymbol, FuncTable, GenericTemplate,
    MethodResolution, MoveStatus, MovedPlace, RTypedField, ReceiverAdjust, StructEntry,
    StructTable, TraitDispatch, TraitEntry, TraitImplEntry, TraitMethodEntry, TraitReceiverShape,
    TraitTable, VariantPayloadResolved, enum_lookup, func_lookup, struct_lookup, template_lookup,
    trait_lookup,
};

mod traits;
pub use traits::{
    ImplResolution, MethodCandidate, find_method_candidates, find_trait_impl_idx_by_span,
    find_trait_impl_method, solve_impl, solve_impl_in_ctx, supertrait_closure,
};
pub(crate) use traits::try_match_against_infer;

mod setup;
use setup::{
    collect_enum_names, collect_funcs, collect_struct_names, collect_trait_names,
    find_trait_impl_idx, push_root_name, resolve_enum_variants,
    resolve_impl_target, resolve_struct_fields, resolve_trait_methods,
    validate_supertrait_obligations,
};

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

// A receiver is a "mutable place" if it's a Var bound to a `mut` local, or a
// FieldAccess chain rooted at one — same rule as for `&mut place` borrows.
pub(crate) fn is_mutable_place(ctx: &CheckCtx, expr: &Expr) -> bool {
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
            ctx.expr_infer_types[expr.id as usize] = Some(ty.clone());
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
                    return Ok(ctx.locals[i].ty.clone());
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
                    Ok(elems[*index as usize].clone())
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
        let tmpl_path = ctx.funcs.templates[template_idx].path.clone();
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
                v.push(ctx.funcs.templates[template_idx].param_types[k].clone());
                k += 1;
            }
            v
        };
        let tmpl_return_type: Option<RType> = ctx.funcs.templates[template_idx]
            .return_type
            .clone();
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
                    ty: fields[k].ty.clone(),
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
    let enum_path_clone = entry.path.clone();
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
        enum_path: enum_path_clone.clone(),
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

pub(crate) fn funcs_entry_index(funcs: &FuncTable, path: &Vec<String>) -> Option<usize> {
    let mut i = 0;
    while i < funcs.entries.len() {
        if &funcs.entries[i].path == path {
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
        VariantPayloadResolved::Tuple(types) => types.clone(),
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
        enum_path: entry.path.clone(),
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
        path: entry.path.clone(),
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
        .map(|e| e.path.clone())
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
        def_field_types.push(entry.fields[k].ty.clone());
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
            let field_ty_raw = entry.fields[i].ty.clone();
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
