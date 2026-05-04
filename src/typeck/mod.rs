use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, Stmt, StructLit, Type,
};
use crate::span::{Error, Span};

mod types;
pub use types::{
    IntKind, LifetimeRepr, RType, byte_size_of, copy_trait_path, drop_trait_path, numeric_lit_op_traits_for_method, flatten_rtype,
    int_kind_name, is_copy_with_bounds, is_drop, is_raw_ptr, is_sized,
    is_variant_payload_uninhabited, needs_drop,
    outer_lifetime, rtype_contains_param, rtype_eq, rtype_to_string, substitute_rtype,
};
use types::{int_kind_from_name, int_kind_max, int_kind_neg_magnitude, int_kind_signed, struct_env};

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
// What an integer-literal type-var is allowed to bind to. After
// dropping numeric literal overloading, literals only resolve to the
// built-in integer types — never to user types via a Num impl. (The
// old behavior allowed `let x: UserType = 42;` when `impl Num for
// UserType` existed; that's now an error.) Param `T` is rejected
// even with `T: Add` bounds, since `T` doesn't carry a `from_i64`
// constructor in the new operator scheme. To use a literal as a
// custom type, write the cast explicitly: `let x = MyType::from(42);`.
fn satisfies_num(
    t: &InferType,
    _traits: &TraitTable,
    _type_params: &Vec<String>,
    _type_param_bounds: &Vec<Vec<Vec<String>>>,
) -> bool {
    matches!(t, InferType::Int(_) | InferType::Var(_))
}

// Whether `t` (an InferType, possibly partially resolved) is `Sized`.
// `Slice(_)` and `Str` are unsized; everything else, including refs to
// DSTs, unresolved Vars/Params, and `!` (zero-sized), is treated as
// Sized. (A Var that later resolves to a DST is unrealistic in
// practice — DSTs don't arise from inference.)
pub(crate) fn is_sized_infer(t: &InferType) -> bool {
    !matches!(t, InferType::Slice(_) | InferType::Str)
}

// InferType counterpart of `concretize_assoc_proj_with_bounds`. Walks
// the InferType, replacing any `AssocProj` whose base resolves enough
// to find a unique impl binding (or a matching `T: Trait<Name = X>`
// constraint on an in-scope type-param). Used at dispatch sites where
// the call result type is an InferType that may carry a projection.
pub(crate) fn infer_concretize_assoc_proj(
    t: &InferType,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bound_assoc: &Vec<Vec<(String, RType)>>,
) -> InferType {
    match t {
        InferType::AssocProj { base, trait_path, name } => {
            let new_base = infer_concretize_assoc_proj(
                base,
                traits,
                type_params,
                type_param_bound_assoc,
            );
            // T::Name via in-scope bound constraint?
            if let InferType::Param(t_name) = &new_base {
                let mut i = 0;
                while i < type_params.len() {
                    if &type_params[i] == t_name && i < type_param_bound_assoc.len() {
                        let mut k = 0;
                        while k < type_param_bound_assoc[i].len() {
                            if &type_param_bound_assoc[i][k].0 == name {
                                let rt = &type_param_bound_assoc[i][k].1;
                                return rtype_to_infer(rt);
                            }
                            k += 1;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            // When the base is still an unresolved Var, leave the
            // projection wrapped (lazy projection). Method dispatch
            // on AssocProj{Var, …} recv unwraps to the inner Var (in
            // `check_method_call`) so chained operations like
            // `1 + 2 + 3` work; AssocProj-vs-concrete unification
            // (in `Subst::unify`) drives the eventual binding when
            // the result reaches a context with a concrete expected
            // type.
            if matches!(new_base, InferType::Var(_)) {
                return InferType::AssocProj {
                    base: Box::new(new_base),
                    trait_path: trait_path.clone(),
                    name: name.clone(),
                };
            }
            let base_rt = infer_to_rtype_for_check(&new_base);
            let candidates = traits::find_assoc_binding(traits, &base_rt, trait_path, name);
            if candidates.len() == 1 {
                rtype_to_infer(&candidates[0])
            } else {
                InferType::AssocProj {
                    base: Box::new(new_base),
                    trait_path: trait_path.clone(),
                    name: name.clone(),
                }
            }
        }
        InferType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(infer_concretize_assoc_proj(
                inner,
                traits,
                type_params,
                type_param_bound_assoc,
            )),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_concretize_assoc_proj(
                inner,
                traits,
                type_params,
                type_param_bound_assoc,
            )),
            mutable: *mutable,
        },
        InferType::Struct { path, type_args, lifetime_args } => {
            let mut new_args: Vec<InferType> = Vec::new();
            for arg in type_args {
                new_args.push(infer_concretize_assoc_proj(
                    arg,
                    traits,
                    type_params,
                    type_param_bound_assoc,
                ));
            }
            InferType::Struct {
                path: path.clone(),
                type_args: new_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        InferType::Enum { path, type_args, lifetime_args } => {
            let mut new_args: Vec<InferType> = Vec::new();
            for arg in type_args {
                new_args.push(infer_concretize_assoc_proj(
                    arg,
                    traits,
                    type_params,
                    type_param_bound_assoc,
                ));
            }
            InferType::Enum {
                path: path.clone(),
                type_args: new_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        InferType::Tuple(elems) => {
            let mut new_elems: Vec<InferType> = Vec::new();
            for e in elems {
                new_elems.push(infer_concretize_assoc_proj(
                    e,
                    traits,
                    type_params,
                    type_param_bound_assoc,
                ));
            }
            InferType::Tuple(new_elems)
        }
        InferType::Slice(inner) => InferType::Slice(Box::new(infer_concretize_assoc_proj(
            inner,
            traits,
            type_params,
            type_param_bound_assoc,
        ))),
        _ => t.clone(),
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
        InferType::Slice(inner) => RType::Slice(Box::new(infer_to_rtype_for_check(inner))),
        InferType::Str => RType::Str,
        InferType::AssocProj { base, trait_path, name } => RType::AssocProj {
            base: Box::new(infer_to_rtype_for_check(base)),
            trait_path: trait_path.clone(),
            name: name.clone(),
        },
        InferType::Never => RType::Never,
        InferType::Char => RType::Char,
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
    lookup_variant_path, place_to_string, resolve_full_path, resolve_type,
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
    // `[T]` — DST. Only valid as the inner of `Ref { inner: Slice(_), .. }`.
    Slice(Box<InferType>),
    // `str` — UTF-8 string DST. Only valid as the inner of a Ref.
    Str,
    // Associated-type projection — InferType counterpart of
    // `RType::AssocProj`. Carries the symbolic base + trait + name
    // until concretization at substitution time.
    AssocProj {
        base: Box<InferType>,
        trait_path: Vec<String>,
        name: String,
    },
    // `!` — InferType counterpart of `RType::Never`. Coerces freely:
    // `unify(Never, _)` succeeds without binding so the other side's
    // inference proceeds. Produced by `break`/`continue`/`return`
    // typecheckers and by calls to functions with `!` return type.
    Never,
    // `char` — InferType counterpart of `RType::Char`.
    Char,
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
        RType::Slice(inner) => InferType::Slice(Box::new(rtype_to_infer(inner))),
        RType::Str => InferType::Str,
        RType::AssocProj { base, trait_path, name } => InferType::AssocProj {
            base: Box::new(rtype_to_infer(base)),
            trait_path: trait_path.clone(),
            name: name.clone(),
        },
        RType::Never => InferType::Never,
        RType::Char => InferType::Char,
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
        InferType::Slice(inner) => InferType::Slice(Box::new(infer_substitute(inner, env))),
        InferType::Str => InferType::Str,
        InferType::AssocProj { base, trait_path, name } => InferType::AssocProj {
            base: Box::new(infer_substitute(base, env)),
            trait_path: trait_path.clone(),
            name: name.clone(),
        },
        InferType::Never => InferType::Never,
        InferType::Char => InferType::Char,
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
        InferType::Slice(inner) => format!("[{}]", infer_to_string(inner)),
        InferType::Str => "str".to_string(),
        InferType::AssocProj { base, trait_path, name } => {
            let trait_disp = if trait_path.is_empty() {
                "?".to_string()
            } else {
                place_to_string(trait_path)
            };
            format!("<{} as {}>::{}", infer_to_string(base), trait_disp, name)
        }
        InferType::Never => "!".to_string(),
        InferType::Char => "char".to_string(),
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
            InferType::Slice(inner) => InferType::Slice(Box::new(self.substitute(inner))),
            InferType::Str => InferType::Str,
            InferType::AssocProj { base, trait_path, name } => InferType::AssocProj {
                base: Box::new(self.substitute(base)),
                trait_path: trait_path.clone(),
                name: name.clone(),
            },
            InferType::Never => InferType::Never,
            InferType::Char => InferType::Char,
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
            // `!` (Never) coerces to every type. Unifying with Never on
            // either side succeeds without binding — the other side's
            // inference proceeds unaffected. This must be checked
            // *before* the (Var, _) / (_, Var) arms, otherwise binding
            // a num-lit Var against Never goes through `bind_var`'s
            // `satisfies_num(Never)` check and fails. Lets e.g.
            // `if cond { break } else { 42 }` type as `i32`: the if's
            // result var unifies first with the then-arm's `!` (no-op)
            // then with the else-arm's i32 var (binds the result).
            (InferType::Never, _) | (_, InferType::Never) => Ok(()),
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
            (InferType::Char, InferType::Char) => Ok(()),
            (InferType::Str, InferType::Str) => Ok(()),
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
            (InferType::Slice(ia), InferType::Slice(ib)) => {
                self.unify(ia.as_ref(), ib.as_ref(), traits, type_params, type_param_bounds, span, file)
            }
            (InferType::Str, InferType::Str) => Ok(()),
            // AssocProj on either side: try to back-propagate. If
            // exactly one impl of `trait_path` has its binding for
            // `name` equal (as an RType) to the other side, unify the
            // projection's base with that impl's target. Handles
            // `<Self as Add>::Output = u32` → bind Self to u32 (since
            // every primitive `impl Add for T` has `Output = T`).
            (InferType::AssocProj { base, trait_path, name }, other)
            | (other, InferType::AssocProj { base, trait_path, name }) => {
                let other_rt = infer_to_rtype_for_check(&other);
                if matches!(other_rt, RType::Param(ref n) if n == "?unknown") {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            infer_to_string(&other),
                            infer_to_string(&InferType::AssocProj { base: base.clone(), trait_path: trait_path.clone(), name: name.clone() })
                        ),
                        span: span.copy(),
                    });
                }
                // If `base` is a num-lit Var, only consider
                // Int-target impls — the Var can only resolve to an
                // int kind, so unrelated user impls (e.g. `impl Add
                // for Wrap { type Output = u32; }`) shouldn't compete
                // with primitive impls. Without this filter, a single
                // user impl breaks `30 + 12 → u32` by returning two
                // candidates with target=u32 (the primitive) and
                // target=Wrap (the user impl).
                let base_is_num_lit_var = matches!(
                    base.as_ref(),
                    InferType::Var(v) if (*v as usize) < self.is_num_lit.len()
                        && self.is_num_lit[*v as usize]
                );
                let mut matching_targets: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < traits.impls.len() {
                    let row = &traits.impls[i];
                    if !trait_path.is_empty() && row.trait_path != trait_path {
                        i += 1;
                        continue;
                    }
                    if base_is_num_lit_var && !matches!(&row.target, RType::Int(_)) {
                        i += 1;
                        continue;
                    }
                    let mut k = 0;
                    while k < row.assoc_type_bindings.len() {
                        if row.assoc_type_bindings[k].0 == name {
                            // `assoc_type_bindings[k].1` may contain
                            // `Param(impl_param)` slots; we only
                            // accept impls whose binding is fully
                            // concrete (no Param) and `rtype_eq` to
                            // other_rt — that matches the
                            // `Output = T` (with T = the impl's
                            // concrete target) primitive case.
                            if !rtype_contains_param(&row.assoc_type_bindings[k].1)
                                && rtype_eq(&row.assoc_type_bindings[k].1, &other_rt)
                            {
                                if !matching_targets
                                    .iter()
                                    .any(|t| rtype_eq(t, &row.target))
                                {
                                    matching_targets.push(row.target.clone());
                                }
                            }
                            break;
                        }
                        k += 1;
                    }
                    i += 1;
                }
                if matching_targets.len() == 1 {
                    let target_infer = rtype_to_infer(&matching_targets[0]);
                    self.unify(base.as_ref(), &target_infer, traits, type_params, type_param_bounds, span, file)
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            infer_to_string(&other),
                            infer_to_string(&InferType::AssocProj { base, trait_path, name })
                        ),
                        span: span.copy(),
                    })
                }
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
            InferType::Slice(inner) => RType::Slice(Box::new(self.finalize(&inner))),
            InferType::Str => RType::Str,
            InferType::AssocProj { base, trait_path, name } => RType::AssocProj {
                base: Box::new(self.finalize(&base)),
                trait_path,
                name,
            },
            InferType::Never => RType::Never,
            InferType::Char => RType::Char,
        }
    }
}

pub(crate) struct LitConstraint {
    var: u32,
    value: u64,
    // `true` for `NegIntLit(value)` — i.e. the source wrote `-value`.
    // The range check requires a signed integer kind whose negative
    // range covers `value`; codegen lowers as `from_i64(-(value as i64))`.
    negative: bool,
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
    // Per-NodeId resolved RType type-args for builtins that need them at
    // codegen (`¤size_of::<T>()`). `None` outside builtin-with-types
    // sites. Finalized into FnSymbol.builtin_type_targets at end-of-fn.
    pub(crate) builtin_type_targets: Vec<Option<Vec<RType>>>,
    // Per-pattern.id ergonomics record (sized to func.node_count).
    // Default-zero means "no auto-peel/binding-override at this pattern
    // node". `check_pattern` writes here when it traverses ref scrutinees
    // with non-ref patterns or applies a non-Move default binding mode.
    pub(crate) pattern_ergo: Vec<PatternErgo>,
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
    // Per-type-param `Trait<Name = X>` constraints from the function's
    // bounds. `[i]` lists `(name, ResolvedRType)` for each constraint
    // on `type_params[i]`'s bounds. Used by AssocProj concretization to
    // resolve `T::Name` projections inside the body.
    pub(crate) type_param_bound_assoc: &'a Vec<Vec<(String, RType)>>,
    // Stack of enclosing loop labels (innermost-last). Each entry is
    // the loop's optional label; `break`/`continue` validate that any
    // referenced label is in this stack.
    pub(crate) loop_labels: Vec<Option<String>>,
    // The enclosing function's declared return type (resolved). Used
    // by `return EXPR;` to unify EXPR against the expected type, and
    // by `?` to verify the function's return is `Result<_, E>` with
    // the same E as the inner Result. `None` only for tail-less fns
    // (return type `()` is `Some(Tuple([]))`).
    pub(crate) fn_return_rt: Option<RType>,
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
                // Mirror collect_funcs's prefix scheme. Path targets
                // use the struct name; non-Path trait impls use
                // `__trait_impl_<idx>`; inherent non-Path impls use
                // `__inherent_synth_<idx>`. Generic-trait impls
                // (trait declares `<T1, …>`) on Path targets append
                // an extra `__trait_impl_<idx>` segment so multiple
                // `impl Trait<X> for Foo` rows don't collide on
                // shared method names.
                let mut method_prefix = path.clone();
                // Span-based lookup is the only one that disambiguates
                // multiple `impl Trait<X> for Foo` rows (path+target
                // matches them all — trait_args differ but aren't keyed
                // here). Setup, borrowck, and codegen all use the
                // span-based variant; typeck mirrors that.
                let trait_impl_idx_opt = if ib.trait_path.is_some() {
                    find_trait_impl_idx_by_span(traits, current_file, &ib.span)
                } else {
                    None
                };
                let trait_is_generic = trait_impl_idx_opt.map_or(false, |idx| {
                    !traits.impls[idx].trait_args.is_empty()
                });
                match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if !p.segments.is_empty() => {
                        method_prefix.push(p.segments[0].name.clone());
                        if trait_is_generic {
                            if let Some(idx) = trait_impl_idx_opt {
                                method_prefix.push(format!("__trait_impl_{}", idx));
                            }
                        }
                    }
                    _ => {
                        if ib.trait_path.is_some() {
                            match trait_impl_idx_opt {
                                Some(idx) => {
                                    method_prefix.push(format!("__trait_impl_{}", idx));
                                }
                                None => unreachable!(
                                    "trait impl with non-Path target must have a registered row"
                                ),
                            }
                        } else {
                            // Inherent impl on a non-Path target.
                            let idx = tables::find_inherent_synth_idx(funcs, current_file, &ib.span)
                                .expect("setup recorded an inherent-synth idx for this impl");
                            method_prefix.push(format!("__inherent_synth_{}", idx));
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
    // Per type-param, collect all `Trait<Name = X>` constraints from
    // the function's bounds (resolved at check time from the AST). Used
    // for `T::Name` projections inside the body.
    let mut type_param_bound_assoc: Vec<Vec<(String, RType)>> = Vec::new();
    {
        let mut idx_offset = 0;
        // Skip impl-level type params (they appear first in
        // type_param_names but their bounds are on the impl, not on
        // `func.type_params`).
        if type_param_names.len() > func.type_params.len() {
            idx_offset = type_param_names.len() - func.type_params.len();
            for _ in 0..idx_offset {
                type_param_bound_assoc.push(Vec::new());
            }
        }
        let mut tp = 0;
        while tp < func.type_params.len() {
            let mut row: Vec<(String, RType)> = Vec::new();
            let mut b = 0;
            while b < func.type_params[tp].bounds.len() {
                let bound = &func.type_params[tp].bounds[b];
                let mut c = 0;
                while c < bound.assoc_constraints.len() {
                    let cname = bound.assoc_constraints[c].name.clone();
                    let cty = resolve_type(
                        &bound.assoc_constraints[c].ty,
                        current_module,
                        structs,
                        enums,
                        self_target,
                        &type_param_names,
                        module_use_scope,
                        reexports,
                        current_file,
                    )?;
                    row.push((cname, cty));
                    c += 1;
                }
                b += 1;
            }
            type_param_bound_assoc.push(row);
            tp += 1;
        }
    }
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
        let rt = concretize_assoc_proj_with_bounds(
            &rt,
            traits,
            &type_param_names,
            &type_param_bound_assoc,
        );
        locals.push(LocalEntry {
            name: func.params[k].name.clone(),
            ty: rtype_to_infer(&rt),
            mutable: false,
        });
        k += 1;
    }
    let return_rt: Option<RType> = match &func.return_type {
        Some(ty) => Some({
            let rt = resolve_type(
                ty,
                current_module,
                structs,
                enums,
                self_target,
                &type_param_names,
                module_use_scope,
                reexports,
                current_file,
            )?;
            concretize_assoc_proj_with_bounds(
                &rt,
                traits,
                &type_param_names,
                &type_param_bound_assoc,
            )
        }),
        None => None,
    };

    let node_count = func.node_count as usize;
    let (expr_infer_types, lit_constraints, method_resolutions, call_resolutions, builtin_type_targets, pattern_ergo, subst) = {
        let mut method_res: Vec<Option<PendingMethodCall>> = Vec::with_capacity(node_count);
        let mut call_res: Vec<Option<PendingCall>> = Vec::with_capacity(node_count);
        let mut expr_infer: Vec<Option<InferType>> = Vec::with_capacity(node_count);
        let mut btt: Vec<Option<Vec<RType>>> = Vec::with_capacity(node_count);
        let mut pat_ergo: Vec<PatternErgo> = Vec::with_capacity(node_count);
        let mut i = 0;
        while i < node_count {
            method_res.push(None);
            call_res.push(None);
            expr_infer.push(None);
            btt.push(None);
            pat_ergo.push(PatternErgo::default());
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
            builtin_type_targets: btt,
            pattern_ergo: pat_ergo,
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
            type_param_bound_assoc: &type_param_bound_assoc,
            reexports,
            use_scope: initial_use_scope,
            loop_labels: Vec::new(),
            fn_return_rt: return_rt.clone(),
        };
        check_block(&mut ctx, &func.body, &return_rt)?;
        (
            ctx.expr_infer_types,
            ctx.lit_constraints,
            ctx.method_resolutions,
            ctx.call_resolutions,
            ctx.builtin_type_targets,
            ctx.pattern_ergo,
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
        if lc.negative {
            if !int_kind_signed(&kind) {
                return Err(Error {
                    file: current_file.to_string(),
                    message: format!(
                        "cannot apply unary `-` to unsigned integer type `{}`",
                        int_kind_name(&kind)
                    ),
                    span: lc.span.copy(),
                });
            }
            if (lc.value as u128) > int_kind_neg_magnitude(&kind) {
                return Err(Error {
                    file: current_file.to_string(),
                    message: format!(
                        "integer literal `-{}` does not fit in `{}`",
                        lc.value,
                        int_kind_name(&kind)
                    ),
                    span: lc.span.copy(),
                });
            }
        } else if (lc.value as u128) > int_kind_max(&kind) {
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
                    Some(td) => {
                        let mut trait_args: Vec<RType> = Vec::new();
                        let mut q = 0;
                        while q < td.trait_arg_infers.len() {
                            trait_args.push(subst.finalize(&td.trait_arg_infers[q]));
                            q += 1;
                        }
                        let recv_type = subst.finalize(&td.recv_type_infer);
                        // If recv_type is concrete and any trait_arg
                        // defaulted (still bound to a Var that
                        // finalize defaulted to i32) without being
                        // unified with a real constraint, prefer the
                        // unique impl for `(trait_path, recv)` —
                        // that's how `1 + 2` against return type u32
                        // works: recv pins to u32 via Output back-prop,
                        // but Rhs's Var only got unified with arg 12's
                        // Var, neither of which got pinned. The impl
                        // table has `impl Add for u32` (Rhs=u32), so
                        // we adopt those trait_args.
                        let recv_for_solve = match &recv_type {
                            RType::Ref { inner, .. } => (**inner).clone(),
                            other => other.clone(),
                        };
                        if !rtype_contains_param(&recv_for_solve)
                            && !trait_args.is_empty()
                        {
                            let mut matches: Vec<Vec<RType>> = Vec::new();
                            let mut r = 0;
                            while r < traits.impls.len() {
                                let row = &traits.impls[r];
                                if row.trait_path != td.trait_path {
                                    r += 1;
                                    continue;
                                }
                                let mut env: Vec<(String, RType)> = Vec::new();
                                if traits::try_match_rtype(&row.target, &recv_for_solve, &mut env) {
                                    let mut concrete_args: Vec<RType> = Vec::new();
                                    let mut a = 0;
                                    while a < row.trait_args.len() {
                                        concrete_args.push(substitute_rtype(&row.trait_args[a], &env));
                                        a += 1;
                                    }
                                    if !concrete_args.iter().any(rtype_contains_param) {
                                        let already = matches.iter().any(|m| {
                                            m.len() == concrete_args.len()
                                                && m.iter().zip(concrete_args.iter()).all(|(x, y)| rtype_eq(x, y))
                                        });
                                        if !already {
                                            matches.push(concrete_args);
                                        }
                                    }
                                }
                                r += 1;
                            }
                            if matches.len() == 1 {
                                trait_args = matches.into_iter().next().unwrap();
                            }
                        }
                        // For trait dispatches that fully concretized
                        // (no `Param` left in recv_type or trait_args),
                        // verify an impl exists. This catches cases
                        // where a trait-arg inference var defaulted to
                        // i32 but no `impl Trait<i32> for Recv` exists,
                        // turning what would otherwise be a codegen-time
                        // unreachable! into a proper user-facing error.
                        let mut needs_validation =
                            !rtype_contains_param(&recv_type);
                        let mut q = 0;
                        while q < trait_args.len() {
                            if rtype_contains_param(&trait_args[q]) {
                                needs_validation = false;
                                break;
                            }
                            q += 1;
                        }
                        if needs_validation {
                            let recv_for_solve = match &recv_type {
                                RType::Ref { inner, .. } => (**inner).clone(),
                                other => other.clone(),
                            };
                            if traits::solve_impl_with_args(
                                &td.trait_path,
                                &trait_args,
                                &recv_for_solve,
                                traits,
                                0,
                            )
                            .is_none()
                            {
                                let mut args_str = String::new();
                                if !trait_args.is_empty() {
                                    args_str.push('<');
                                    let mut q = 0;
                                    while q < trait_args.len() {
                                        if q > 0 {
                                            args_str.push_str(", ");
                                        }
                                        args_str.push_str(&rtype_to_string(&trait_args[q]));
                                        q += 1;
                                    }
                                    args_str.push('>');
                                }
                                return Err(Error {
                                    file: current_file.to_string(),
                                    message: format!(
                                        "no impl of `{}{}` for `{}` (cannot pick method `{}`)",
                                        place_to_string(&td.trait_path),
                                        args_str,
                                        rtype_to_string(&recv_for_solve),
                                        td.method_name
                                    ),
                                    span: td.dispatch_span.copy(),
                                });
                            }
                        }
                        Some(TraitDispatch {
                            trait_path: td.trait_path.clone(),
                            trait_args,
                            method_name: td.method_name.clone(),
                            recv_type,
                        })
                    }
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
        funcs.entries[e].builtin_type_targets = builtin_type_targets;
        funcs.entries[e].pattern_ergo = pattern_ergo;
    } else {
        let mut t = 0;
        while t < funcs.templates.len() {
            if funcs.templates[t].path == full {
                funcs.templates[t].expr_types = expr_types;
                funcs.templates[t].method_resolutions = method_resolutions;
                funcs.templates[t].call_resolutions = call_resolutions;
                funcs.templates[t].builtin_type_targets = builtin_type_targets;
                funcs.templates[t].pattern_ergo = pattern_ergo;
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

pub(crate) struct PendingTraitDispatch {
    pub(crate) trait_path: Vec<String>,
    // Positional trait-args as InferTypes (may include fresh vars
    // pending unification). Empty for non-generic-trait dispatches.
    pub(crate) trait_arg_infers: Vec<InferType>,
    pub(crate) method_name: String,
    pub(crate) recv_type_infer: InferType,
    // Call site span — used to attribute the post-finalize "no impl
    // matches the resolved trait_args" error when an unresolved/
    // wrong-defaulted trait-arg leaves codegen no impl to pick.
    pub(crate) dispatch_span: Span,
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
// True iff `block` contains a statement-level expression that
// diverges (its expression-type resolves to `!`). Used by let-else
// to recognize the natural `else { return …; }` form as diverging
// even though the block's tail-type is `()` — the diverging expr
// carries a trailing `;`, becoming a Stmt::Expr whose inner
// expression's recorded type is `!`. Type-driven so future
// `!`-typed expressions (calls to `!`-returning functions, etc.)
// are picked up automatically without enumerating ASTNode kinds.
fn block_has_diverging_stmt(ctx: &CheckCtx, block: &Block) -> bool {
    let mut i = 0;
    while i < block.stmts.len() {
        if let Stmt::Expr(e) = &block.stmts[i] {
            let id = e.id as usize;
            if let Some(t) = ctx.expr_infer_types.get(id).and_then(|o| o.as_ref()) {
                if matches!(ctx.subst.substitute(t), InferType::Never) {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

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
    // Overwrite the recorded type at the value expr's id with the
    // final type (in case an annotation pinned it down). Codegen
    // reads expr_types[value.id] to size the binding's storage.
    ctx.expr_infer_types[let_stmt.value.id as usize] = Some(final_ty.clone());
    // Type-check the pattern against the value's type and collect
    // bindings into `ctx.locals` so subsequent statements can see
    // them. Refutable patterns require `else` (let-else); the
    // irrefutability check is shared with match-exhaustiveness — a
    // single pattern is irrefutable iff it alone exhausts the
    // scrutinee type, which `pattern_is_irrefutable` decides.
    let mut bindings: Vec<(String, InferType, Span, bool)> = Vec::new();
    check_pattern(ctx, &let_stmt.pattern, &final_ty, &mut bindings)?;
    if let_stmt.else_block.is_none()
        && !patterns::pattern_is_irrefutable(ctx, &final_ty, &let_stmt.pattern)
    {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "refutable pattern in `let` binding (use `let … else { … }` if the pattern can fail)".to_string(),
            span: let_stmt.pattern.span.copy(),
        });
    }
    if let Some(else_block) = &let_stmt.else_block {
        // The else block must diverge. Two cases count: the block's
        // tail expression is `!`-typed (e.g. `return x` without
        // trailing `;`), OR one of its statements is a diverging
        // expression-statement (`return …;`, `break;`, `continue;`,
        // `panic!(…);`). Without the second case the natural
        // spelling `else { return 42; }` would be rejected because
        // a stmt-with-`;` block has tail-type `()`.
        // The pattern's bindings are NOT in scope inside else.
        let else_ty = check_block_inner(ctx, else_block.as_ref())?;
        let resolved = ctx.subst.substitute(&else_ty);
        let diverges = matches!(resolved, InferType::Never)
            || block_has_diverging_stmt(ctx, else_block.as_ref());
        if !diverges {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "`let-else` block must diverge (type `!`), found `{}`",
                    infer_to_string(&resolved)
                ),
                span: else_block.span.copy(),
            });
        }
    }
    let mut k = 0;
    while k < bindings.len() {
        ctx.locals.push(LocalEntry {
            name: bindings[k].0.clone(),
            ty: bindings[k].1.clone(),
            mutable: bindings[k].3,
        });
        k += 1;
    }
    Ok(())
}


fn check_assign_stmt(ctx: &mut CheckCtx, assign: &AssignStmt) -> Result<(), Error> {
    // Two flavors of LHS:
    //   1. Var-rooted chain: `x` or `x.f.g.h`.
    //   2. Deref-rooted chain: `*p` or `(*p).f.g.h`.
    if let Some((root_expr, fields)) = extract_deref_rooted_chain(&assign.lhs) {
        return check_deref_rooted_assign(ctx, root_expr, &fields, assign);
    }
    // 3. Index LHS (`arr[idx] = val`). Typecheck the LHS for its
    //    Output type, then unify rhs against that. Codegen handles
    //    the IndexMut dispatch + store-through.
    if let ExprKind::Index { .. } = &assign.lhs.kind {
        let lhs_ty = check_expr(ctx, &assign.lhs)?;
        let rhs_ty = check_expr(ctx, &assign.rhs)?;
        ctx.subst.unify(
            &rhs_ty,
            &lhs_ty,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &assign.span,
            ctx.current_file,
        )?;
        return Ok(());
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
            // Smart-pointer write via `DerefMut`. The LHS type is
            // the impl's `Target` (declared on the supertrait
            // `Deref`); codegen routes the write through
            // `<X as DerefMut>::deref_mut(&mut x)` and stores into
            // the returned `&mut Target`.
            let inner_rt = infer_to_rtype_for_check(&other);
            let deref_mut_path = vec![
                "std".to_string(),
                "ops".to_string(),
                "DerefMut".to_string(),
            ];
            let deref_path = vec![
                "std".to_string(),
                "ops".to_string(),
                "Deref".to_string(),
            ];
            let has_deref_mut =
                traits::solve_impl(&deref_mut_path, &inner_rt, ctx.traits, 0).is_some();
            let target_candidates =
                traits::find_assoc_binding(ctx.traits, &inner_rt, &deref_path, "Target");
            if has_deref_mut && target_candidates.len() == 1 {
                rtype_to_infer(&target_candidates[0])
            } else {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot dereference `{}` for assignment",
                        infer_to_string(&other)
                    ),
                    span: assign.lhs.span.copy(),
                });
            }
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
                negative: false,
                span: expr.span.copy(),
            });
            Ok(InferType::Var(v))
        }
        ExprKind::NegIntLit(n) => {
            let v = ctx.subst.fresh_int();
            ctx.lit_constraints.push(LitConstraint {
                var: v,
                value: *n,
                negative: true,
                span: expr.span.copy(),
            });
            Ok(InferType::Var(v))
        }
        ExprKind::StrLit(_) => {
            // String literal is `&'static str`. Lifetime is `'static`
            // because the data lives in the module's data section
            // for the lifetime of the program.
            Ok(InferType::Ref {
                inner: Box::new(InferType::Str),
                mutable: false,
                lifetime: LifetimeRepr::Named("static".to_string()),
            })
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
        ExprKind::CharLit(_) => Ok(InferType::Char),
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
        ExprKind::For(f) => check_for_expr(ctx, f, expr),
        ExprKind::Break { label, label_span } => {
            check_loop_label(ctx, label, label_span, &expr.span)?;
            // `break` diverges — type as `!` so it can sit as one arm
            // of an `if`/`match` whose other arm yields a real value.
            Ok(InferType::Never)
        }
        ExprKind::Continue { label, label_span } => {
            check_loop_label(ctx, label, label_span, &expr.span)?;
            Ok(InferType::Never)
        }
        ExprKind::Return { value } => check_return_expr(ctx, value.as_deref(), expr),
        ExprKind::Try { inner, question_span } => check_try_expr(ctx, inner, question_span, expr),
        ExprKind::Index { base, index, bracket_span } => {
            check_index_expr(ctx, base, index, bracket_span, expr)
        }
        ExprKind::MacroCall { name, name_span, args } => {
            check_macro_call(ctx, name, name_span, args)
        }
    }
}

// `panic!(msg: &str)` is the only macro recognized so far. Type-checks
// the single `&str` argument and yields `!` (the macro diverges via
// the `env.panic` host call).
fn check_macro_call(
    ctx: &mut CheckCtx,
    name: &str,
    name_span: &Span,
    args: &Vec<Expr>,
) -> Result<InferType, Error> {
    if name != "panic" {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("unknown macro `{}!`", name),
            span: name_span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `panic!`: expected 1, got {}",
                args.len()
            ),
            span: name_span.copy(),
        });
    }
    let arg_ty = check_expr(ctx, &args[0])?;
    let str_ref = InferType::Ref {
        inner: Box::new(InferType::Str),
        mutable: false,
        lifetime: LifetimeRepr::Inferred(0),
    };
    ctx.subst.unify(
        &arg_ty,
        &str_ref,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    Ok(InferType::Never)
}

// `arr[idx]` — typecheck base + index, look up the `Index` impl on
// base's type (handling autoderef of `&T`/`&mut T` so `(&v)[idx]`
// works), unify idx with `usize`, and yield the impl's `Output`
// associated type. Codegen branches on enclosing context to decide
// whether to call `index` or `index_mut`.
fn check_index_expr(
    ctx: &mut CheckCtx,
    base: &Expr,
    index: &Expr,
    bracket_span: &Span,
    _expr: &Expr,
) -> Result<InferType, Error> {
    let base_ty = check_expr(ctx, base)?;
    let resolved_base = ctx.subst.substitute(&base_ty);
    // Autoderef through references for the trait lookup. `&Vec<u32>`
    // and `Vec<u32>` both index the same way; the codegen handles the
    // ref by passing it through unchanged.
    let lookup_ty = match &resolved_base {
        InferType::Ref { inner, .. } => (**inner).clone(),
        other => other.clone(),
    };
    // The index expression's type drives which `Index<Idx>` impl
    // we look up (`Idx = usize` for element indexing, `Idx = Range<usize>`
    // etc. for slicing). For unconstrained integer literals — the
    // bare-int `v[0]` case AND nested ones like `s[1..4]` whose
    // `Range<?int>` wraps unbound int vars — default every still-loose
    // int-class var inside the idx type to `usize` before dispatch so
    // the common shape (`Index<usize>` / `Index<Range<usize>>`) keeps
    // working without explicit `0usize` / `1usize..4usize` annotations.
    let idx_ty = check_expr(ctx, index)?;
    default_int_vars_to_usize(ctx, &idx_ty, &index.span)?;
    let idx_rt = infer_to_rtype_for_check(&ctx.subst.substitute(&idx_ty));
    let lookup_rt = infer_to_rtype_for_check(&lookup_ty);
    let index_path = vec!["std".to_string(), "ops".to_string(), "Index".to_string()];
    let resolution = traits::solve_impl_with_args(
        &index_path,
        &vec![idx_rt.clone()],
        &lookup_rt,
        ctx.traits,
        0,
    );
    let resolution = match resolution {
        Some(r) => r,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "the type `{}` cannot be indexed by `{}` (no matching `Index<{}>` impl)",
                    rtype_to_string(&lookup_rt),
                    rtype_to_string(&idx_rt),
                    rtype_to_string(&idx_rt)
                ),
                span: bracket_span.copy(),
            });
        }
    };
    // Read the resolved impl's `Output` binding and substitute the
    // impl's type-params using the resolution's subst.
    let impl_row = &ctx.traits.impls[resolution.impl_idx];
    let mut output_rt: Option<RType> = None;
    let mut k = 0;
    while k < impl_row.assoc_type_bindings.len() {
        if impl_row.assoc_type_bindings[k].0 == "Output" {
            output_rt = Some(substitute_rtype(
                &impl_row.assoc_type_bindings[k].1,
                &resolution.subst,
            ));
            break;
        }
        k += 1;
    }
    let output_rt = output_rt.ok_or_else(|| Error {
        file: ctx.current_file.to_string(),
        message: format!(
            "internal: `Index<{}> for {}` impl missing `Output` binding",
            rtype_to_string(&idx_rt),
            rtype_to_string(&lookup_rt)
        ),
        span: bracket_span.copy(),
    })?;
    Ok(rtype_to_infer(&output_rt))
}

// Walk an `InferType`, defaulting every still-unbound integer-class
// `Var` to `usize`. Used at index sites so naked `arr[0]` and
// `s[1..4]` (whose `Range<?int>` argument has unbound int vars
// inside) pick `Index<usize>` / `Index<Range<usize>>` rather than
// failing dispatch because `?int` won't have defaulted to `i32`
// until end-of-fn.
fn default_int_vars_to_usize(
    ctx: &mut CheckCtx,
    ty: &InferType,
    span: &Span,
) -> Result<(), Error> {
    let resolved = ctx.subst.substitute(ty);
    match &resolved {
        InferType::Var(v) => {
            if (*v as usize) < ctx.subst.is_num_lit.len() && ctx.subst.is_num_lit[*v as usize] {
                ctx.subst.unify(
                    ty,
                    &InferType::Int(IntKind::Usize),
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    span,
                    ctx.current_file,
                )?;
            }
            Ok(())
        }
        InferType::Struct { type_args, .. } | InferType::Enum { type_args, .. } => {
            for a in type_args {
                default_int_vars_to_usize(ctx, a, span)?;
            }
            Ok(())
        }
        InferType::Ref { inner, .. } | InferType::RawPtr { inner, .. } => {
            default_int_vars_to_usize(ctx, inner, span)
        }
        InferType::Tuple(elems) => {
            for e in elems {
                default_int_vars_to_usize(ctx, e, span)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// `return EXPR` / `return`. EXPR (or `()` if absent) unifies against
// the enclosing function's declared return type. The whole `return`
// expression has type `!` so it can sit anywhere a value is expected
// without constraining surrounding inference.
fn check_return_expr(
    ctx: &mut CheckCtx,
    value: Option<&Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    let expected_rt = match &ctx.fn_return_rt {
        Some(rt) => rt.clone(),
        None => RType::Tuple(Vec::new()),
    };
    let expected = rtype_to_infer(&expected_rt);
    let actual = match value {
        Some(e) => check_expr(ctx, e)?,
        None => InferType::Tuple(Vec::new()),
    };
    let span = match value {
        Some(e) => e.span.copy(),
        None => expr.span.copy(),
    };
    ctx.subst.unify(
        &actual,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &span,
        ctx.current_file,
    )?;
    Ok(InferType::Never)
}

// `expr?` — typecheck the inner as `Result<T, E>`, require the
// enclosing function's return type to be `Result<U, E>` with the same
// `E`, and yield `T`. No early desugar — codegen lowers this directly
// so the `?` token's span carries through diagnostics.
fn check_try_expr(
    ctx: &mut CheckCtx,
    inner: &Expr,
    question_span: &Span,
    expr: &Expr,
) -> Result<InferType, Error> {
    let inner_ty = check_expr(ctx, inner)?;
    let resolved = ctx.subst.substitute(&inner_ty);
    // Inner must be `std::result::Result<T, E>`. (No general `Try`
    // trait yet — we hardcode the canonical Result path.)
    let result_path = vec!["std".to_string(), "result".to_string(), "Result".to_string()];
    let (ok_ty, err_ty) = match &resolved {
        InferType::Enum { path, type_args, .. }
            if path == &result_path && type_args.len() == 2 =>
        {
            (type_args[0].clone(), type_args[1].clone())
        }
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "the `?` operator requires a `Result`, got `{}`",
                    infer_to_string(&resolved)
                ),
                span: question_span.copy(),
            });
        }
    };
    // The enclosing function must return `Result<_, E_fn>` with the
    // same E. Look at fn_return_rt; if it's not a Result-shaped enum,
    // reject.
    let fn_ret_rt = match &ctx.fn_return_rt {
        Some(rt) => rt.clone(),
        None => RType::Tuple(Vec::new()),
    };
    let (_fn_ok, fn_err) = match &fn_ret_rt {
        RType::Enum { path, type_args, .. }
            if path == &result_path && type_args.len() == 2 =>
        {
            (type_args[0].clone(), type_args[1].clone())
        }
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "the `?` operator can only be used in a function returning `Result`; this function returns `{}`",
                    rtype_to_string(&fn_ret_rt)
                ),
                span: question_span.copy(),
            });
        }
    };
    // Unify inner E with function's E. Mismatch → "incompatible
    // error type" diagnostic.
    let fn_err_infer = rtype_to_infer(&fn_err);
    if let Err(e) = ctx.subst.unify(
        &err_ty,
        &fn_err_infer,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        question_span,
        ctx.current_file,
    ) {
        // Re-wrap with a `?`-specific message.
        let _ = e;
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "the `?` operator's error type `{}` doesn't match the function's `{}`",
                infer_to_string(&err_ty),
                rtype_to_string(&fn_err)
            ),
            span: question_span.copy(),
        });
    }
    let _ = expr;
    Ok(ok_ty)
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

// `for pat in iter { body }`. The iter expression's resolved type
// must implement `std::iter::Iterator`; the pattern is checked
// against the impl's `Item` binding, the body must be `()`-typed,
// and the loop expression itself yields `()`. The loop's label is
// stacked just like `while` so `break`/`continue` (with optional
// label) work inside the body.
fn check_for_expr(
    ctx: &mut CheckCtx,
    f: &crate::ast::ForLoop,
    expr: &Expr,
) -> Result<InferType, Error> {
    if let Some(name) = &f.label {
        let mut i = ctx.loop_labels.len();
        while i > 0 {
            i -= 1;
            if ctx.loop_labels[i].as_deref() == Some(name.as_str()) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("duplicate loop label `'{}`", name),
                    span: f
                        .label_span
                        .as_ref()
                        .map(|s| s.copy())
                        .unwrap_or_else(|| expr.span.copy()),
                });
            }
        }
    }
    // Type-check the iter expression and resolve its type.
    let iter_ty = check_expr(ctx, &f.iter)?;
    let resolved_iter = ctx.subst.substitute(&iter_ty);
    let iter_rt = infer_to_rtype_for_check(&resolved_iter);
    let iterator_path = vec![
        "std".to_string(),
        "iter".to_string(),
        "Iterator".to_string(),
    ];
    // Resolve `<iter_ty as Iterator>::Item`.
    let item_candidates = traits::find_assoc_binding(
        ctx.traits,
        &iter_rt,
        &iterator_path,
        "Item",
    );
    if item_candidates.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "the trait `Iterator` is not implemented for `{}` (required by `for` loop)",
                rtype_to_string(&iter_rt)
            ),
            span: f.iter.span.copy(),
        });
    }
    if item_candidates.len() > 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "multiple `Iterator` impls for `{}` — `for` loop is ambiguous",
                rtype_to_string(&iter_rt)
            ),
            span: f.iter.span.copy(),
        });
    }
    let item_ty = rtype_to_infer(&item_candidates[0]);
    // Check the pattern against `Item` and collect bindings for the
    // body's scope.
    let mark = ctx.locals.len();
    let mut bindings: Vec<(String, InferType, Span, bool)> = Vec::new();
    check_pattern(ctx, &f.pattern, &item_ty, &mut bindings)?;
    let mut k = 0;
    while k < bindings.len() {
        ctx.locals.push(LocalEntry {
            name: bindings[k].0.clone(),
            ty: bindings[k].1.clone(),
            mutable: bindings[k].3,
        });
        k += 1;
    }
    ctx.loop_labels.push(f.label.clone());
    let unit = InferType::Tuple(Vec::new());
    let body_ty = check_block_inner(ctx, f.body.as_ref())?;
    ctx.subst.unify(
        &body_ty,
        &unit,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &f.body.span,
        ctx.current_file,
    )?;
    ctx.loop_labels.pop();
    ctx.locals.truncate(mark);
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
    // The if's overall type is the non-`!` arm's type when one arm
    // diverges (so `if cond { panic!() } else { 42 }` types as the
    // else arm's u32, not `!`). When neither arm is `!`, returning
    // either is fine — they unified.
    let resolved_then = ctx.subst.substitute(&then_ty);
    let resolved_else = ctx.subst.substitute(&else_ty);
    let result = match (&resolved_then, &resolved_else) {
        (InferType::Never, _) => else_ty,
        _ => then_ty,
    };
    let _ = resolved_then;
    let _ = resolved_else;
    Ok(result)
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
        "size_of" => return check_builtin_size_of(ctx, type_args, args, expr),
        "make_slice" => return check_builtin_make_slice(ctx, type_args, args, expr, false),
        "make_mut_slice" => return check_builtin_make_slice(ctx, type_args, args, expr, true),
        "slice_len" => return check_builtin_slice_len(ctx, type_args, args, expr),
        "slice_ptr" => return check_builtin_slice_ptr(ctx, type_args, args, expr, false),
        "slice_mut_ptr" => return check_builtin_slice_ptr(ctx, type_args, args, expr, true),
        "str_len" => return check_builtin_str_len(ctx, type_args, args, expr),
        "str_as_bytes" => return check_builtin_str_as_bytes(ctx, type_args, args, expr, false),
        "str_as_mut_bytes" => return check_builtin_str_as_bytes(ctx, type_args, args, expr, true),
        "make_str" => return check_builtin_make_str(ctx, type_args, args, expr, false),
        "make_mut_str" => return check_builtin_make_str(ctx, type_args, args, expr, true),
        "ptr_usize_add" | "ptr_usize_sub" => {
            return check_builtin_ptr_usize_offset(ctx, name, type_args, args, expr);
        }
        "ptr_isize_offset" => {
            return check_builtin_ptr_isize_offset(ctx, type_args, args, expr);
        }
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

// `¤str_len(s: &str) -> usize`. Pulls the length half out of the
// fat ref. Same codegen as `¤slice_len` (drops ptr, keeps len) but
// takes no type-arg since `str`'s element type is fixed.
fn check_builtin_str_len(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "builtin `¤str_len` does not take type arguments".to_string(),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤str_len` takes 1 argument (`&str`), got {}",
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let arg_ty = check_expr(ctx, &args[0])?;
    // Accept either `&str` or `&mut str` — length read is mutability-
    // agnostic. Mirrors `¤slice_len`'s behaviour for `&[T]`/`&mut [T]`.
    let resolved = ctx.subst.substitute(&arg_ty);
    let ok = matches!(
        &resolved,
        InferType::Ref { inner, .. } if matches!(inner.as_ref(), InferType::Str)
    );
    if !ok {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤str_len` first argument must be `&str` or `&mut str`, got `{}`",
                infer_to_string(&resolved)
            ),
            span: args[0].span.copy(),
        });
    }
    Ok(rtype_to_infer(&RType::Int(IntKind::Usize)))
}

// `¤str_as_bytes(s: &str) -> &[u8]` (mutable=false) and
// `¤str_as_mut_bytes(s: &mut str) -> &mut [u8]` (mutable=true). The
// fat-ref representation of `&str`/`&mut str` and `&[u8]`/`&mut [u8]`
// is bit-identical (both are (ptr, len) over u8 bytes), so codegen
// is a pure pass-through.
fn check_builtin_str_as_bytes(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
    mutable: bool,
) -> Result<InferType, Error> {
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "builtin `¤str_as_bytes` does not take type arguments".to_string(),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤str_as_bytes` takes 1 argument (`&str`), got {}",
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let arg_ty = check_expr(ctx, &args[0])?;
    let expected = InferType::Ref {
        inner: Box::new(InferType::Str),
        mutable,
        lifetime: LifetimeRepr::Inferred(0),
    };
    ctx.subst.unify(
        &arg_ty,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    Ok(InferType::Ref {
        inner: Box::new(InferType::Slice(Box::new(InferType::Int(IntKind::U8)))),
        mutable,
        lifetime: LifetimeRepr::Inferred(0),
    })
}

// `¤make_str(ptr: *const u8, len: usize) -> &str` (mutable=false) and
// `¤make_mut_str(ptr: *mut u8, len: usize) -> &mut str` (mutable=true).
// Constructs a fat `&str`/`&mut str` from raw parts. UTF-8 invariant
// is the caller's responsibility (unenforced). Codegen is a pure
// no-op — args already form the fat ref.
fn check_builtin_make_str(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
    mutable: bool,
) -> Result<InferType, Error> {
    let name = if mutable { "make_mut_str" } else { "make_str" };
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤{}` does not take type arguments", name),
            span: expr.span.copy(),
        });
    }
    if args.len() != 2 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤{}` takes 2 arguments (ptr, len), got {}",
                name,
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let arg0_ty = check_expr(ctx, &args[0])?;
    let arg1_ty = check_expr(ctx, &args[1])?;
    let expected0 = rtype_to_infer(&RType::RawPtr {
        inner: Box::new(RType::Int(IntKind::U8)),
        mutable,
    });
    ctx.subst.unify(
        &arg0_ty,
        &expected0,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    let expected1 = rtype_to_infer(&RType::Int(IntKind::Usize));
    ctx.subst.unify(
        &arg1_ty,
        &expected1,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[1].span,
        ctx.current_file,
    )?;
    Ok(InferType::Ref {
        inner: Box::new(InferType::Str),
        mutable,
        lifetime: LifetimeRepr::Inferred(0),
    })
}

// `¤slice_ptr::<T>(s: &[T]) -> *const T` and the mut variant
// `¤slice_mut_ptr::<T>(s: &mut [T]) -> *mut T`. Pulls the data-ptr
// half out of the fat ref. Codegen drops the length scalar (top of
// stack) and keeps the ptr scalar (below it). The mutable variant
// has the same wasm shape — only the typeck input/output differ.
fn check_builtin_slice_ptr(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
    mutable: bool,
) -> Result<InferType, Error> {
    let name = if mutable { "slice_mut_ptr" } else { "slice_ptr" };
    if type_args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤{}` takes 1 type argument (`T`), got {}",
                name,
                type_args.len()
            ),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤{}` takes 1 argument, got {}",
                name,
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let t = resolve_type(
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
    let arg_ty = check_expr(ctx, &args[0])?;
    let expected = InferType::Ref {
        inner: Box::new(InferType::Slice(Box::new(rtype_to_infer(&t)))),
        mutable,
        lifetime: LifetimeRepr::Inferred(0),
    };
    ctx.subst.unify(
        &arg_ty,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    Ok(InferType::RawPtr {
        inner: Box::new(rtype_to_infer(&t)),
        mutable,
    })
}

// `¤slice_len::<T>(s: &[T]) -> usize`. Pulls the length half out of
// the fat ref. Codegen drops the data ptr from the wasm stack and
// keeps the length scalar.
fn check_builtin_slice_len(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if type_args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤slice_len` takes 1 type argument (`T`), got {}",
                type_args.len()
            ),
            span: expr.span.copy(),
        });
    }
    if args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤slice_len` takes 1 argument (`&[T]`), got {}",
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let t = resolve_type(
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
    let arg_ty = check_expr(ctx, &args[0])?;
    // Accept either `&[T]` or `&mut [T]` — the length read is the
    // same regardless of mutability, and `get_mut` needs to read len
    // through `&mut self` without an extra intrinsic.
    let resolved = ctx.subst.substitute(&arg_ty);
    let inner_ok = match &resolved {
        InferType::Ref { inner, .. } => match inner.as_ref() {
            InferType::Slice(_) => true,
            _ => false,
        },
        _ => false,
    };
    if !inner_ok {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤slice_len` first argument must be `&[T]` or `&mut [T]`, got `{}`",
                infer_to_string(&resolved)
            ),
            span: args[0].span.copy(),
        });
    }
    // Unify the inner element type with the supplied turbofish T —
    // mutability is allowed to differ.
    if let InferType::Ref { inner, mutable, .. } = &resolved {
        if let InferType::Slice(element) = inner.as_ref() {
            ctx.subst.unify(
                element.as_ref(),
                &rtype_to_infer(&t),
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                &args[0].span,
                ctx.current_file,
            )?;
            let _ = mutable;
        }
    }
    Ok(rtype_to_infer(&RType::Int(IntKind::Usize)))
}

// `¤make_slice::<T>(ptr: *const u8, len: usize) -> &[T]`. Constructs a
// fat slice ref from an existing data pointer and a length. The
// pointer is taken as `*const u8` so the same intrinsic call site
// works regardless of T's size (the caller is expected to have already
// computed bytes-worth offsets); `T` then determines only the slice's
// element type. Used by `Vec<T>::as_slice` to surface the buffer.
// Codegen is a pure no-op — both args are already i32s on the wasm
// stack, which is exactly the fat-ref representation.
fn check_builtin_make_slice(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
    mutable: bool,
) -> Result<InferType, Error> {
    let name = if mutable { "make_mut_slice" } else { "make_slice" };
    if type_args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤{}` takes 1 type argument (`T`), got {}",
                name,
                type_args.len()
            ),
            span: expr.span.copy(),
        });
    }
    if args.len() != 2 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤{}` takes 2 arguments (ptr, len), got {}",
                name,
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let t = resolve_type(
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
    let arg0_ty = check_expr(ctx, &args[0])?;
    let arg1_ty = check_expr(ctx, &args[1])?;
    let expected0 = rtype_to_infer(&RType::RawPtr {
        inner: Box::new(RType::Int(IntKind::U8)),
        mutable,
    });
    ctx.subst.unify(
        &arg0_ty,
        &expected0,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    let expected1 = rtype_to_infer(&RType::Int(IntKind::Usize));
    ctx.subst.unify(
        &arg1_ty,
        &expected1,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[1].span,
        ctx.current_file,
    )?;
    Ok(InferType::Ref {
        inner: Box::new(InferType::Slice(Box::new(rtype_to_infer(&t)))),
        mutable,
        lifetime: LifetimeRepr::Inferred(0),
    })
}

// `¤size_of::<T>() -> usize`. Mandatory turbofish (no inference). The
// result is a compile-time-known constant — at codegen time T is
// concrete (after monomorphization) and `byte_size_of(T)` decides the
// emitted `i32.const`.
fn check_builtin_size_of(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if type_args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤size_of` takes 1 type argument (`T`), got {}",
                type_args.len()
            ),
            span: expr.span.copy(),
        });
    }
    if !args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤size_of` takes 0 arguments, got {}",
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    // Resolve T and stash on the per-NodeId artifact so codegen can
    // compute byte_size_of(T) at the call site (substituted through the
    // mono env if T is a Param).
    let t = resolve_type(
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
    ctx.builtin_type_targets[expr.id as usize] = Some(vec![t]);
    Ok(rtype_to_infer(&RType::Int(IntKind::Usize)))
}

// `¤ptr_usize_add(p, n)` and `¤ptr_usize_sub(p, n)`: byte-wise pointer
// arithmetic. `p` must be `*const T` or `*mut T`; `n` is `usize`. The
// result keeps the input's mutability and pointee type. Use these as
// the building block for higher-level methods (`std::primitive::pointer`).
fn check_builtin_ptr_usize_offset(
    ctx: &mut CheckCtx,
    name: &str,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤{}` does not take type arguments", name),
            span: expr.span.copy(),
        });
    }
    if args.len() != 2 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("builtin `¤{}` takes 2 arguments, got {}", name, args.len()),
            span: expr.span.copy(),
        });
    }
    let arg0_ty = check_expr(ctx, &args[0])?;
    let arg1_ty = check_expr(ctx, &args[1])?;
    let resolved = ctx.subst.substitute(&arg0_ty);
    let (mutable, inner) = match &resolved {
        InferType::RawPtr { mutable, inner } => (*mutable, (**inner).clone()),
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "builtin `¤{}` first argument must be a raw pointer, got `{}`",
                    name,
                    infer_to_string(&resolved)
                ),
                span: args[0].span.copy(),
            });
        }
    };
    let expected = rtype_to_infer(&RType::Int(IntKind::Usize));
    ctx.subst.unify(
        &arg1_ty,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[1].span,
        ctx.current_file,
    )?;
    Ok(InferType::RawPtr {
        inner: Box::new(inner),
        mutable,
    })
}

// `¤ptr_isize_offset(p, n)`: signed-byte pointer offset. Same shape as
// the usize variants but takes an `isize` so callers can shift in
// either direction in one call.
fn check_builtin_ptr_isize_offset(
    ctx: &mut CheckCtx,
    type_args: &Vec<crate::ast::Type>,
    args: &Vec<Expr>,
    expr: &Expr,
) -> Result<InferType, Error> {
    if !type_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "builtin `¤ptr_isize_offset` does not take type arguments".to_string(),
            span: expr.span.copy(),
        });
    }
    if args.len() != 2 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "builtin `¤ptr_isize_offset` takes 2 arguments, got {}",
                args.len()
            ),
            span: expr.span.copy(),
        });
    }
    let arg0_ty = check_expr(ctx, &args[0])?;
    let arg1_ty = check_expr(ctx, &args[1])?;
    let resolved = ctx.subst.substitute(&arg0_ty);
    let (mutable, inner) = match &resolved {
        InferType::RawPtr { mutable, inner } => (*mutable, (**inner).clone()),
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "builtin `¤ptr_isize_offset` first argument must be a raw pointer, got `{}`",
                    infer_to_string(&resolved)
                ),
                span: args[0].span.copy(),
            });
        }
    };
    let expected = rtype_to_infer(&RType::Int(IntKind::Isize));
    ctx.subst.unify(
        &arg1_ty,
        &expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[1].span,
        ctx.current_file,
    )?;
    Ok(InferType::RawPtr {
        inner: Box::new(inner),
        mutable,
    })
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
    MethodResolution, MoveStatus, MovedPlace, PatternErgo, RTypedField, ReceiverAdjust,
    StructEntry, StructTable, SupertraitRef, TraitDispatch, TraitEntry, TraitImplEntry,
    TraitMethodEntry, TraitReceiverShape, TraitTable, VariantPayloadResolved, enum_lookup,
    find_inherent_synth_idx, func_lookup, struct_lookup, template_lookup, trait_lookup,
};

mod traits;
pub use traits::{
    MethodCandidate, concretize_assoc_proj,
    concretize_assoc_proj_with_bounds, find_assoc_binding, find_method_candidates,
    find_trait_impl_idx_by_span, find_trait_impl_method, solve_impl,
    solve_impl_with_args, supertrait_closure,
};
pub(crate) use traits::try_match_against_infer;

mod setup;
use setup::{
    collect_enum_names, collect_funcs, collect_struct_names, collect_trait_names,
    push_root_name, resolve_enum_variants,
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

// Whether `expr` is a place expression that supports mutation. Covers
// the same shapes as `*p = …;` and `vec[i] = …;` assignments — Var
// (mut binding or `&mut T`), Var-rooted field/tuple-index chains,
// `*p` for `&mut T`/`*mut T`, and `e[i]` when `e` is a mutable place
// and the recv's type implements `IndexMut` (so the dispatch can
// route through the `&mut Self` autoref level for `e[i] OP= rhs;`).
pub(crate) fn is_mutable_place(ctx: &CheckCtx, expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Var(name) => {
            let mut i = ctx.locals.len();
            while i > 0 {
                i -= 1;
                if ctx.locals[i].name == *name {
                    if ctx.locals[i].mutable {
                        return true;
                    }
                    let resolved = ctx.subst.substitute(&ctx.locals[i].ty);
                    return matches!(resolved, InferType::Ref { mutable: true, .. });
                }
            }
            false
        }
        ExprKind::FieldAccess(fa) => is_mutable_place(ctx, &fa.base),
        ExprKind::TupleIndex { base, .. } => is_mutable_place(ctx, base),
        ExprKind::Deref(inner) => {
            // Look at the inner expression's recorded type. If it
            // resolves to `&mut T` or `*mut T`, the deref is a
            // mutable place. (Reading the type from `expr_infer_types`
            // requires the inner expr to have been checked first;
            // method dispatch's `is_mutable_place` runs after
            // `check_expr(receiver)` for the call's recv.)
            let inner_ty_opt = ctx.expr_infer_types
                .get(inner.id as usize)
                .cloned()
                .flatten();
            if let Some(ty) = inner_ty_opt {
                let resolved = ctx.subst.substitute(&ty);
                matches!(
                    resolved,
                    InferType::Ref { mutable: true, .. }
                        | InferType::RawPtr { mutable: true, .. }
                )
            } else {
                false
            }
        }
        ExprKind::Index { base, .. } => {
            // `base[idx]` is a mutable place if `base` itself is a
            // mutable place (so we can take `&mut base`) and the
            // base's type implements `IndexMut`. We don't run a full
            // trait-resolution check here; the dispatch path's own
            // candidate match for `index_mut` will handle the
            // type-side test, and emitting the autoref-mut level is
            // safe even if no IndexMut impl exists (the call simply
            // won't dispatch). Keeping this conservative on
            // base-mutability matches the assignment rule for
            // `vec[i] = …;`.
            is_mutable_place(ctx, base)
        }
        _ => false,
    }
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
pub(super) fn check_place_expr(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
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
                other => {
                    // Smart-pointer place: route through `Deref` /
                    // `DerefMut` (caller decides which). The place's
                    // type is the impl's `Target`.
                    let deref_path = vec![
                        "std".to_string(),
                        "ops".to_string(),
                        "Deref".to_string(),
                    ];
                    let inner_rt = infer_to_rtype_for_check(&other);
                    let candidates = traits::find_assoc_binding(
                        ctx.traits,
                        &inner_rt,
                        &deref_path,
                        "Target",
                    );
                    if candidates.len() == 1 {
                        return Ok(rtype_to_infer(&candidates[0]));
                    }
                    Err(Error {
                        file: ctx.current_file.to_string(),
                        message: format!(
                            "cannot dereference `{}` — type does not implement `Deref`",
                            infer_to_string(&other)
                        ),
                        span: expr.span.copy(),
                    })
                }
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
        other => {
            // Smart-pointer deref via `std::ops::Deref`. When the
            // inner type isn't a built-in ref/raw-ptr, look up
            // `<inner_ty as Deref>::Target` — if a single impl
            // matches, use its Target type and let codegen route
            // the deref through `Deref::deref`.
            let deref_path = vec![
                "std".to_string(),
                "ops".to_string(),
                "Deref".to_string(),
            ];
            let inner_rt = infer_to_rtype_for_check(&other);
            let candidates = traits::find_assoc_binding(
                ctx.traits,
                &inner_rt,
                &deref_path,
                "Target",
            );
            if candidates.len() == 1 {
                return Ok(rtype_to_infer(&candidates[0]));
            }
            Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot dereference `{}` — type does not implement `Deref`",
                    infer_to_string(&other)
                ),
                span: deref_expr.span.copy(),
            })
        }
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
    let target_is_char = matches!(&target, RType::Char);
    if !target_is_ptr && !target_is_int && !target_is_char {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "casts are only allowed to raw pointer, integer, or `char` types, got `{}`",
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
    } else if target_is_char {
        // `as char` only valid from `u8` (Rust's exact rule). Other
        // ints would need range-checking; pocket-rust skips the check
        // and accepts anything int-typed for now — codegen treats
        // both as i32.
        matches!(&resolved_src, InferType::Int(_) | InferType::Var(_))
    } else {
        // Int target: source must be an integer, an unbound integer
        // var, a raw pointer (the `*T as usize` round-trip is the
        // only ergonomic way to compare addresses, e.g. `p.is_null()`),
        // or a `char` (`'X' as u32` is the canonical char→int
        // conversion). At codegen, raw pointers, integers ≤ 32 bits,
        // and `char` all flatten to wasm `i32`.
        matches!(
            &resolved_src,
            InferType::Int(_) | InferType::Var(_) | InferType::RawPtr { .. } | InferType::Char
        )
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
            type_var_ids: var_ids.clone(),
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
        // Static enforcement of `Trait<Name = T>` bound constraints.
        // Each type-arg the call inferred for the template's type-params
        // must satisfy every `<Name = T>` constraint on its bounds:
        // looking up the impl of the bound trait for the inferred
        // type, the impl's binding for `Name` must equal `T`.
        let tmpl_bounds = ctx.funcs.templates[template_idx].type_param_bounds.clone();
        let tmpl_bound_assoc =
            ctx.funcs.templates[template_idx].type_param_bound_assoc.clone();
        let tmpl_type_params = ctx.funcs.templates[template_idx].type_params.clone();
        // Build a substitution env mapping each template type-param to
        // its inferred RType so we can substitute the assoc-constraint's
        // expected type before comparing it against the impl's actual
        // binding. Without this, `fn double<T: Add<T, Output = T>>` at
        // call site `double::<u32>(21)` compares the bound's `T`
        // (unsubstituted) against the impl's `u32` and reports a bogus
        // mismatch.
        let mut subst_env: Vec<(String, RType)> = Vec::new();
        let mut q = 0;
        while q < var_ids.len() && q < tmpl_type_params.len() {
            let inferred = ctx.subst.substitute(&InferType::Var(var_ids[q]));
            let inferred_rt = infer_to_rtype_for_check(&inferred);
            subst_env.push((tmpl_type_params[q].clone(), inferred_rt));
            q += 1;
        }
        let mut p = 0;
        while p < var_ids.len() {
            if p >= tmpl_bounds.len() {
                p += 1;
                continue;
            }
            let inferred = ctx.subst.substitute(&InferType::Var(var_ids[p]));
            let inferred_rt = infer_to_rtype_for_check(&inferred);
            let mut b = 0;
            while b < tmpl_bounds[p].len() {
                let trait_path = &tmpl_bounds[p][b];
                let constraints = if b < tmpl_bound_assoc[p].len() {
                    &tmpl_bound_assoc[p][b]
                } else {
                    p += 1;
                    continue;
                };
                if constraints.is_empty() {
                    b += 1;
                    continue;
                }
                let mut c = 0;
                while c < constraints.len() {
                    let (cname, cty_expected_raw) = &constraints[c];
                    // Substitute under inferred type-args before
                    // comparison — `Output = T` in the bound becomes
                    // `Output = u32` when T is inferred to u32.
                    let cty_expected = substitute_rtype(cty_expected_raw, &subst_env);
                    let actual_candidates = traits::find_assoc_binding(
                        ctx.traits,
                        &inferred_rt,
                        trait_path,
                        cname,
                    );
                    if actual_candidates.is_empty() {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message: format!(
                                "the trait bound `{}: {}` is not satisfied (no impl found to satisfy `{} = {}`)",
                                rtype_to_string(&inferred_rt),
                                place_to_string(trait_path),
                                cname,
                                rtype_to_string(&cty_expected),
                            ),
                            span: call_expr.span.copy(),
                        });
                    }
                    if actual_candidates.len() > 1
                        || !rtype_eq(&actual_candidates[0], &cty_expected)
                    {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message: format!(
                                "type mismatch on associated type `{}::{}`: expected `{}`, got `{}` (from `impl {} for {}`)",
                                place_to_string(trait_path),
                                cname,
                                rtype_to_string(&cty_expected),
                                rtype_to_string(&actual_candidates[0]),
                                place_to_string(trait_path),
                                rtype_to_string(&inferred_rt),
                            ),
                            span: call_expr.span.copy(),
                        });
                    }
                    c += 1;
                }
                b += 1;
            }
            p += 1;
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
