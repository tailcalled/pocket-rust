use crate::ast::{
    AssignStmt, Block, Call, Closure, Expr, ExprKind, FieldAccess, Function, Item, LetStmt,
    Module, Stmt, StructLit, Type,
};
use crate::span::{Error, Span};

mod types;
pub use types::{
    DynBound, IntKind, LifetimeRepr, RType, byte_size_of, copy_trait_path, drop_trait_path,
    dyn_bound_eq, dyn_bounds_eq, int_kind_from_name, numeric_lit_op_traits_for_method,
    flatten_rtype, int_kind_name, is_copy, is_copy_with_bounds, is_drop, is_raw_ptr, is_sized,
    is_variant_payload_uninhabited, needs_drop, outer_lifetime, peel_opaque,
    rtype_contains_param, rtype_eq, rtype_to_string, subst_and_peel, substitute_rtype,
};
use types::{int_kind_max, int_kind_neg_magnitude, int_kind_signed, struct_env};

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
        InferType::Opaque { fn_path, slot } => RType::Opaque {
            fn_path: fn_path.clone(),
            slot: *slot,
        },
        InferType::FnPtr { params, ret } => RType::FnPtr {
            params: params.iter().map(infer_to_rtype_for_check).collect(),
            ret: Box::new(infer_to_rtype_for_check(ret)),
        },
        InferType::Dyn { bounds, lifetime } => RType::Dyn {
            bounds: bounds.iter().map(infer_dyn_bound_to_rtype_for_check).collect(),
            lifetime: lifetime.clone(),
        },
    }
}

fn infer_dyn_bound_to_rtype_for_check(b: &InferDynBound) -> DynBound {
    DynBound {
        trait_path: b.trait_path.clone(),
        trait_args: b.trait_args.iter().map(infer_to_rtype_for_check).collect(),
        assoc_bindings: b
            .assoc_bindings
            .iter()
            .map(|(n, t)| (n.clone(), infer_to_rtype_for_check(t)))
            .collect(),
    }
}


// Returns indices of every param whose outermost ref lifetime equals
// `target`. Empty if no param matches. Phase D returns multiple matches:
// when `'a` ties multiple ref params to the return, all those args'
// borrows propagate into the result (the "combined borrow sets" rule).
mod lifetimes;
pub use lifetimes::{find_lifetime_source, lifetime_in_scope};
pub mod variance;
pub use variance::Variance;
use lifetimes::{
    find_elision_source, freshen_inferred_lifetimes, require_no_inferred_lifetimes,
    validate_named_lifetimes,
};

pub mod use_scope;
pub use use_scope::{
    ReExportTable, UseEntry, accessor_crate_of, build_reexport_table, field_visible_from,
    flatten_use_tree, func_path_resolved, is_visible_from, module_use_entries,
    resolve_via_reexports, resolve_via_use_scopes, struct_lookup_resolved,
    trait_lookup_resolved,
};

mod path_resolve;

pub mod dyn_safety;
pub use path_resolve::{
    lookup_variant_path, place_to_string, resolve_full_path, resolve_type,
    segments_to_string,
};

// ----- Inference machinery -----

pub fn check(
    root: &Module,
    structs: &mut StructTable,
    enums: &mut EnumTable,
    aliases: &mut AliasTable,
    traits: &mut TraitTable,
    funcs: &mut FuncTable,
    consts: &mut ConstTable,
    reexports: &mut ReExportTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    // Crate name is the root module's name, captured once at entry —
    // empty for the user crate, "std" for the stdlib library, etc. We
    // thread this through so submodules don't have to infer the crate
    // root from path[0] (which is wrong for user-crate submodules,
    // whose path[0] is a submodule name, not the crate name).
    let root_crate_name: &str = root.name.as_str();
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
    collect_struct_names(root, &mut path, root_crate_name, structs)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_enum_names(root, &mut path, root_crate_name, enums)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_trait_names(root, &mut path, root_crate_name, traits)?;

    // Resolve type aliases before struct/enum field resolution so a
    // field type can reference an alias. Aliases themselves resolve in
    // declaration order; an alias whose target references another
    // alias must come *after* it in source. Cycle detection: not yet
    // implemented; a simple recursive case would loop. Selfhost's
    // aliases are all flat (target = primitive), so the gap is
    // theoretical for now.
    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_type_aliases(root, &mut path, root_crate_name, aliases, structs, enums, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_struct_fields(root, &mut path, root_crate_name, structs, enums, aliases, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_enum_variants(root, &mut path, root_crate_name, enums, structs, aliases, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_trait_methods(root, &mut path, root_crate_name, traits, structs, enums, aliases, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_consts(root, &mut path, root_crate_name, consts, structs, enums, aliases, reexports)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_funcs(root, &mut path, root_crate_name, funcs, next_idx, structs, enums, aliases, traits, reexports)?;

    validate_supertrait_obligations(traits)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    let mut current_file = root.source_file.clone();
    check_module(root, &mut path, root_crate_name, &mut current_file, structs, enums, aliases, traits, funcs, consts, reexports)?;

    // RPIT: each `impl Trait` slot was pinned during its function's
    // body check (`record_rpit_pin` in `check_block_with_expected`).
    // No post-typeck finalize pass runs here — `RType::Opaque{fn,slot}`
    // is a stable indirection through typeck's tables. Consumers
    // outside typeck (mono, codegen, layout) call `peel_opaque` (in
    // `types.rs`) to resolve to the slot's pin at the boundary; this
    // avoids the "remember every RType-holding table to walk" pattern
    // a single in-place rewrite would impose.

    // Register a unit `StructEntry` for each closure discovered during
    // typeck so borrowck/codegen can look up the synthesized type. The
    // matching `Item::Struct` + `Item::Impl Fn<...>` AST nodes are
    // emitted by the closure-lowering pass run after typeck — see the
    // `closures-and-fn-traits` skill.
    register_closure_structs(structs, funcs);

    // Use-site variance inference over user (and synthesized closure)
    // structs/enums. Borrowck consults the resulting variance vectors
    // at each value-flow boundary between same-path types with
    // different region/type args, to decide whether to emit a one-way
    // outlives constraint (Covariant) or to equate (Invariant).
    variance::compute_variance(structs, enums);

    Ok(())
}

fn register_closure_structs(structs: &mut StructTable, funcs: &FuncTable) {
    let mut e = 0;
    while e < funcs.entries.len() {
        let entry = &funcs.entries[e];
        let mut i = 0;
        while i < entry.closures.len() {
            if let Some(ci) = &entry.closures[i] {
                push_closure_struct(structs, ci);
            }
            i += 1;
        }
        e += 1;
    }
    let mut t = 0;
    while t < funcs.templates.len() {
        let tmpl = &funcs.templates[t];
        let mut i = 0;
        while i < tmpl.closures.len() {
            if let Some(ci) = &tmpl.closures[i] {
                push_closure_struct(structs, ci);
            }
            i += 1;
        }
        t += 1;
    }
}

// Register a synthesized closure impl — entry point used by
// `closure_lower::lower` to install the post-typeck `impl Fn<(...)> for
// __closure_<id>` AST node into the typeck tables. Mirrors the
// per-impl steps performed by `setup::collect_funcs` (resolve target,
// resolve trait ref, register impl row, register impl method, validate
// signatures), then runs `check_function` on the synthesized method
// body so its expr_types/method_resolutions/etc are populated for
// codegen.
pub fn register_synthesized_closure_impl(
    ib: &crate::ast::ImplBlock,
    parent_module_path: &Vec<String>,
    crate_name: &str,
    source_file: &str,
    structs: &mut StructTable,
    enums: &mut EnumTable,
    aliases: &mut AliasTable,
    traits: &mut TraitTable,
    funcs: &mut FuncTable,
    consts: &ConstTable,
    reexports: &mut ReExportTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    // Use scope is empty — the synthesized impl uses fully-qualified
    // paths (`std::ops::Fn`, the closure's own struct path) so no
    // resolution against an enclosing module's `use` declarations is
    // needed.
    let use_scope: Vec<use_scope::UseEntry> = Vec::new();
    // Resolve the impl's target (a unit struct registered at end of
    // typeck::check via `register_closure_structs`).
    let target_rt = setup::resolve_impl_target(
        ib,
        parent_module_path,
        structs,
        enums,
        aliases,
        &use_scope,
        reexports,
        source_file,
    )?;
    // Synthesized closure impls inherit the enclosing fn's type-
    // params (via `ImplBlock.type_params`). Without this propagation,
    // a closure inside `fn helper<T>(...)` would synthesize an impl
    // with no type-params, and the method body's `T` references would
    // fail to resolve at the synthesized method's typeck — see rt3
    // problem 5.
    let impl_type_params: Vec<String> = ib
        .type_params
        .iter()
        .map(|p| p.name.clone())
        .collect();
    // Phase 2B: synthesized closure impls carry a `'cap` lifetime
    // parameter when the struct has any non-Copy capture fields. Read
    // it from the ImplBlock AST that `closure_lower::synthesize_impl_for_closure`
    // built — propagating to the param-types validation so the
    // `&self: &__closure_<id><'cap>` recv type's `'cap` arg passes
    // `validate_named_lifetimes`.
    let impl_lifetime_params: Vec<String> = ib
        .lifetime_params
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let impl_type_param_bounds: Vec<Vec<Vec<String>>> = ib
        .type_params
        .iter()
        .map(|_| Vec::new())
        .collect();

    // Resolve the trait ref (Fn) and validate the impl shape against
    // the trait's declared methods + assoc types.
    let trait_path_node = ib.trait_path.as_ref().expect(
        "synthesized closure impls always carry a trait_path",
    );
    let (trait_full, trait_args) = setup::resolve_trait_ref(
        trait_path_node,
        parent_module_path,
        structs,
        enums,
        aliases,
        Some(&target_rt),
        &impl_type_params,
        traits,
        &use_scope,
        reexports,
        source_file,
    )?;
    setup::validate_trait_impl(ib, &trait_full, traits, source_file)?;
    let assoc_bindings = setup::resolve_and_validate_assoc_bindings(
        ib,
        &trait_full,
        &target_rt,
        parent_module_path,
        structs,
        enums,
        aliases,
        traits,
        &impl_type_params,
        &use_scope,
        reexports,
        source_file,
    )?;

    let trait_impl_idx = traits.impls.len();
    setup::register_trait_impl(
        ib,
        &trait_full,
        trait_args,
        target_rt.clone(),
        &impl_type_params,
        &impl_lifetime_params,
        &impl_type_param_bounds,
        assoc_bindings,
        traits,
        source_file,
    )?;

    // Method-path prefix mirrors what `setup::collect_funcs` produces
    // for a Path-target trait impl: `<parent_module>::<target_first_segment>`.
    // Closure structs aren't generic-trait impls so no
    // `__trait_impl_<idx>` segment.
    let target_first_seg = match &ib.target.kind {
        crate::ast::TypeKind::Path(p) if !p.segments.is_empty() => {
            p.segments[0].name.clone()
        }
        _ => {
            return Err(Error {
                file: source_file.to_string(),
                message: "internal: closure impl target must be a Path".to_string(),
                span: ib.span.copy(),
            });
        }
    };
    let mut method_prefix = parent_module_path.clone();
    method_prefix.push(target_first_seg);
    // Generic-trait impls (whose trait carries positional args, e.g.
    // `Fn<(P0,)>`) need a per-row `__trait_impl_<idx>` segment so
    // multiple impls of the same trait family on the same target can
    // coexist. Mirrors the scheme `setup::collect_funcs` uses for
    // ordinary trait impls and the lookup `borrowck::check_module`
    // does at every Item::Impl walk.
    let trait_is_generic = !traits.impls[trait_impl_idx].trait_args.is_empty();
    if trait_is_generic {
        method_prefix.push(format!("__trait_impl_{}", trait_impl_idx));
    }

    // Register the call method.
    let mut k = 0;
    while k < ib.methods.len() {
        setup::register_function(
            &ib.methods[k],
            parent_module_path,
            crate_name,
            &method_prefix,
            Some(&target_rt),
            &impl_type_params,
            &impl_lifetime_params,
            &impl_type_param_bounds,
            &Vec::new(),
            Some(trait_impl_idx),
            funcs,
            next_idx,
            structs,
            enums,
            aliases,
            traits,
            &use_scope,
            reexports,
            source_file,
        )?;
        k += 1;
    }

    // Validate that the impl's method signatures match the trait's.
    setup::validate_trait_impl_signatures(
        ib,
        &trait_full,
        &traits.impls[trait_impl_idx].trait_args,
        &target_rt,
        &method_prefix,
        funcs,
        traits,
        source_file,
    )?;

    // Type-check each method body. `check_function` reads the
    // already-registered FnSymbol/Template, so registration above must
    // come first.
    let mut k = 0;
    while k < ib.methods.len() {
        check_function(
            &ib.methods[k],
            parent_module_path,
            crate_name,
            &method_prefix,
            Some(&target_rt),
            source_file,
            structs,
            enums,
            aliases,
            traits,
            funcs,
            consts,
            reexports,
            &use_scope,
        )?;
        k += 1;
    }
    Ok(())
}

// Look up the `Fn`-family bound on a generic param, if one exists,
// and extract its expected closure signature (param types + return
// type). Returns None when the param has no Fn/FnMut/FnOnce bound, or
// the bound's args/Output aren't fully concrete (any inference would
// need to flow the other direction).
//
// The bound `Fn(P0, P1) -> R` is stored as `(["std", "ops", "Fn"],
// [Tuple([P0, P1])])` for the path/args, plus `("Output", R)` in the
// assoc-constraints list. We unwrap the tuple's elements to give the
// closure's expected per-param types.
fn lookup_fn_bound_signature(
    param_name: &str,
    type_params: &Vec<String>,
    bound_paths: &Vec<Vec<Vec<String>>>,
    bound_args: &Vec<Vec<Vec<RType>>>,
    bound_assoc: &Vec<Vec<Vec<(String, RType)>>>,
) -> Option<(Vec<InferType>, InferType)> {
    let mut idx: Option<usize> = None;
    let mut i = 0;
    while i < type_params.len() {
        if type_params[i] == param_name {
            idx = Some(i);
            break;
        }
        i += 1;
    }
    let idx = idx?;
    if idx >= bound_paths.len() {
        return None;
    }
    let bounds = &bound_paths[idx];
    let args_rows = bound_args.get(idx);
    let assoc_rows = bound_assoc.get(idx);
    let mut b = 0;
    while b < bounds.len() {
        let path = &bounds[b];
        let is_fn_family = path.len() == 3
            && path[0] == "std"
            && path[1] == "ops"
            && (path[2] == "Fn" || path[2] == "FnMut" || path[2] == "FnOnce");
        if is_fn_family {
            // Args: positional bound arg 0 is the (P0, P1, ...) tuple.
            let trait_args = args_rows.and_then(|r| r.get(b))?;
            if trait_args.is_empty() {
                return None;
            }
            let params: Vec<InferType> = match &trait_args[0] {
                RType::Tuple(elems) => elems.iter().map(rtype_to_infer).collect(),
                _ => return None,
            };
            // Output: assoc-constraint binding for "Output".
            let return_ty = assoc_rows
                .and_then(|r| r.get(b))
                .and_then(|constraints| {
                    constraints
                        .iter()
                        .find(|(name, _)| name == "Output")
                        .map(|(_, rt)| rtype_to_infer(rt))
                })?;
            return Some((params, return_ty));
        }
        b += 1;
    }
    None
}

fn push_closure_struct(structs: &mut StructTable, ci: &ClosureInfo) {
    // Skip if already registered (defensive — shouldn't happen given
    // unique per-counter idx, but guards against re-runs).
    let mut k = 0;
    while k < structs.entries.len() {
        if structs.entries[k].path == ci.synthesized_struct_path {
            return;
        }
        k += 1;
    }
    // One field per capture, in first-reference order. Copy captures
    // (`CaptureMode::Move`) become value-typed fields; non-Copy
    // captures (`CaptureMode::Ref`) become `&'cap T` fields and the
    // synthesized struct gets a `'cap` lifetime parameter so the field
    // type passes pocket-rust's "refs in struct fields require a
    // named lifetime" check.
    let mut fields: Vec<RTypedField> = Vec::new();
    let mut needs_cap_lifetime = false;
    let mut i = 0;
    while i < ci.captures.len() {
        let ty = match ci.captures[i].mode {
            CaptureMode::Move => ci.captures[i].captured_ty.clone(),
            CaptureMode::Ref => {
                needs_cap_lifetime = true;
                RType::Ref {
                    inner: Box::new(ci.captures[i].captured_ty.clone()),
                    mutable: false,
                    lifetime: LifetimeRepr::Named("cap".to_string()),
                }
            }
            CaptureMode::RefMut => {
                needs_cap_lifetime = true;
                RType::Ref {
                    inner: Box::new(ci.captures[i].captured_ty.clone()),
                    mutable: true,
                    lifetime: LifetimeRepr::Named("cap".to_string()),
                }
            }
        };
        fields.push(RTypedField {
            name: ci.captures[i].binding_name.clone(),
            name_span: ci.body_span.copy(),
            ty,
            vis: ResolvedVisibility::Public,
        });
        i += 1;
    }
    let lifetime_params = if needs_cap_lifetime {
        vec!["cap".to_string()]
    } else {
        Vec::new()
    };
    let tpv = vec![Variance::Covariant; ci.enclosing_type_params.len()];
    let lpv = vec![Variance::Covariant; lifetime_params.len()];
    structs.entries.push(StructEntry {
        path: ci.synthesized_struct_path.clone(),
        name_span: ci.body_span.copy(),
        file: ci.source_file.clone(),
        type_params: ci.enclosing_type_params.clone(),
        lifetime_params,
        type_param_variance: tpv,
        lifetime_param_variance: lpv,
        fields,
        vis: ResolvedVisibility::Public,
    });
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
    // Existential `impl Trait` — InferType counterpart of
    // `RType::Opaque`. Each `impl Trait` occurrence in the return
    // type gets a distinct `slot`; trait dispatch consults
    // `FnSymbol.rpit_slots[slot].bounds`, structure-level operations
    // resolve via `rpit_slots[slot].pin`.
    Opaque {
        fn_path: Vec<String>,
        slot: u32,
    },
    // InferType counterpart of `RType::FnPtr`. Inference flows
    // through param/ret slots structurally — unification matches arity
    // and recurses, with `Never`/`Var` coercing per the usual rules.
    FnPtr {
        params: Vec<InferType>,
        ret: Box<InferType>,
    },
    // InferType counterpart of `RType::Dyn`. Mirrors the same bound
    // structure (trait_path, trait_args, assoc_bindings) so dispatch +
    // coercion can read trait args / assoc bindings during typeck
    // before everything finalizes back to `RType::Dyn`.
    Dyn {
        bounds: Vec<InferDynBound>,
        lifetime: LifetimeRepr,
    },
}

#[derive(Clone)]
pub struct InferDynBound {
    pub trait_path: Vec<String>,
    pub trait_args: Vec<InferType>,
    pub assoc_bindings: Vec<(String, InferType)>,
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
        RType::Opaque { fn_path, slot } => InferType::Opaque {
            fn_path: fn_path.clone(),
            slot: *slot,
        },
        RType::FnPtr { params, ret } => InferType::FnPtr {
            params: params.iter().map(rtype_to_infer).collect(),
            ret: Box::new(rtype_to_infer(ret)),
        },
        RType::Dyn { bounds, lifetime } => InferType::Dyn {
            bounds: bounds.iter().map(rtype_dyn_bound_to_infer).collect(),
            lifetime: lifetime.clone(),
        },
    }
}

fn infer_dyn_bound_eq(a: &InferDynBound, b: &InferDynBound) -> bool {
    if a.trait_path != b.trait_path {
        return false;
    }
    if a.trait_args.len() != b.trait_args.len() {
        return false;
    }
    let mut i = 0;
    while i < a.trait_args.len() {
        if !infer_eq(&a.trait_args[i], &b.trait_args[i]) {
            return false;
        }
        i += 1;
    }
    if a.assoc_bindings.len() != b.assoc_bindings.len() {
        return false;
    }
    let mut i = 0;
    while i < a.assoc_bindings.len() {
        if a.assoc_bindings[i].0 != b.assoc_bindings[i].0
            || !infer_eq(&a.assoc_bindings[i].1, &b.assoc_bindings[i].1)
        {
            return false;
        }
        i += 1;
    }
    true
}

fn infer_dyn_bounds_eq(a: &Vec<InferDynBound>, b: &Vec<InferDynBound>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !infer_dyn_bound_eq(&a[i], &b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

// Best-effort InferType equality — used only for dyn bound comparison.
// Doesn't unify or substitute through Subst; callers should already
// have substituted as needed.
fn infer_eq(a: &InferType, b: &InferType) -> bool {
    use InferType::*;
    match (a, b) {
        (Var(va), Var(vb)) => va == vb,
        (Int(ka), Int(kb)) => ka == kb,
        (Bool, Bool) | (Str, Str) | (Never, Never) | (Char, Char) => true,
        (Param(na), Param(nb)) => na == nb,
        (Tuple(ea), Tuple(eb)) => {
            ea.len() == eb.len() && ea.iter().zip(eb.iter()).all(|(x, y)| infer_eq(x, y))
        }
        (Struct { path: pa, type_args: aa, .. }, Struct { path: pb, type_args: ab, .. }) => {
            pa == pb && aa.len() == ab.len() && aa.iter().zip(ab.iter()).all(|(x, y)| infer_eq(x, y))
        }
        (Enum { path: pa, type_args: aa, .. }, Enum { path: pb, type_args: ab, .. }) => {
            pa == pb && aa.len() == ab.len() && aa.iter().zip(ab.iter()).all(|(x, y)| infer_eq(x, y))
        }
        (Ref { inner: ia, mutable: ma, .. }, Ref { inner: ib, mutable: mb, .. }) => {
            ma == mb && infer_eq(ia, ib)
        }
        (RawPtr { inner: ia, mutable: ma }, RawPtr { inner: ib, mutable: mb }) => {
            ma == mb && infer_eq(ia, ib)
        }
        (Slice(ia), Slice(ib)) => infer_eq(ia, ib),
        (AssocProj { base: ba, trait_path: ta, name: na }, AssocProj { base: bb, trait_path: tb, name: nb }) => {
            ta == tb && na == nb && infer_eq(ba, bb)
        }
        (Opaque { fn_path: pa, slot: sa }, Opaque { fn_path: pb, slot: sb }) => pa == pb && sa == sb,
        (FnPtr { params: pa, ret: ra }, FnPtr { params: pb, ret: rb }) => {
            pa.len() == pb.len() && pa.iter().zip(pb.iter()).all(|(x, y)| infer_eq(x, y)) && infer_eq(ra, rb)
        }
        (Dyn { bounds: ba, .. }, Dyn { bounds: bb, .. }) => infer_dyn_bounds_eq(ba, bb),
        _ => false,
    }
}

fn rtype_dyn_bound_to_infer(b: &DynBound) -> InferDynBound {
    InferDynBound {
        trait_path: b.trait_path.clone(),
        trait_args: b.trait_args.iter().map(rtype_to_infer).collect(),
        assoc_bindings: b
            .assoc_bindings
            .iter()
            .map(|(n, t)| (n.clone(), rtype_to_infer(t)))
            .collect(),
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
        InferType::Opaque { fn_path, slot } => InferType::Opaque {
            fn_path: fn_path.clone(),
            slot: *slot,
        },
        InferType::FnPtr { params, ret } => InferType::FnPtr {
            params: params.iter().map(|p| infer_substitute(p, env)).collect(),
            ret: Box::new(infer_substitute(ret, env)),
        },
        InferType::Dyn { bounds, lifetime } => InferType::Dyn {
            bounds: bounds.clone(),
            lifetime: lifetime.clone(),
        },
    }
}

// Resolve a `Type` AST that may contain `_` placeholders, allocating
// a fresh inference variable per placeholder. Pre-walk replaces each
// `Placeholder` with a synthetic `Path` segment named `__infer_<id>`,
// then `resolve_type` runs with those names in scope (so they resolve
// to `RType::Param`), then a substitution maps each synth Param to a
// fresh `InferType::Var`. Used at turbofish call sites and `let`
// annotations — the only positions that accept `_`.
pub(crate) fn resolve_type_to_infer(
    ctx: &mut CheckCtx,
    ty: &crate::ast::Type,
) -> Result<InferType, Error> {
    let mut synth_names: Vec<String> = Vec::new();
    let unique_prefix: u32 = ctx.subst.fresh_var();
    let rewritten = rewrite_placeholders(ty, &mut synth_names, unique_prefix);
    let mut extended_params: Vec<String> = ctx.type_params.clone();
    let mut k = 0;
    while k < synth_names.len() {
        extended_params.push(synth_names[k].clone());
        k += 1;
    }
    let rt = path_resolve::resolve_type(
        &rewritten,
        ctx.current_module,
        ctx.structs,
        ctx.enums,
        ctx.aliases,
        ctx.self_target,
        &extended_params,
        &ctx.use_scope,
        ctx.reexports,
        ctx.current_file,
    )?;
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut k = 0;
    while k < synth_names.len() {
        env.push((synth_names[k].clone(), InferType::Var(ctx.subst.fresh_var())));
        k += 1;
    }
    Ok(infer_substitute(&rtype_to_infer(&rt), &env))
}

// Walk a Type AST, replacing each `TypeKind::Placeholder` with a
// synthesized `Path` segment whose name is `__infer_<prefix>_<idx>`.
// Records each synth name into `synth_names`. Recurses into the
// composite type kinds.
fn rewrite_placeholders(
    ty: &crate::ast::Type,
    synth_names: &mut Vec<String>,
    prefix: u32,
) -> crate::ast::Type {
    use crate::ast::{Path, PathSegment, Type, TypeKind};
    let kind = match &ty.kind {
        TypeKind::Placeholder => {
            let name = format!("__infer_{}_{}", prefix, synth_names.len());
            synth_names.push(name.clone());
            TypeKind::Path(Path {
                segments: vec![PathSegment {
                    name,
                    span: ty.span.copy(),
                    lifetime_args: Vec::new(),
                    args: Vec::new(),
                }],
                span: ty.span.copy(),
            })
        }
        TypeKind::Path(p) => {
            let mut new_segs: Vec<PathSegment> = Vec::new();
            let mut i = 0;
            while i < p.segments.len() {
                let s = &p.segments[i];
                let mut new_args: Vec<Type> = Vec::new();
                let mut j = 0;
                while j < s.args.len() {
                    new_args.push(rewrite_placeholders(&s.args[j], synth_names, prefix));
                    j += 1;
                }
                new_segs.push(PathSegment {
                    name: s.name.clone(),
                    span: s.span.copy(),
                    lifetime_args: s.lifetime_args.clone(),
                    args: new_args,
                });
                i += 1;
            }
            TypeKind::Path(Path { segments: new_segs, span: p.span.copy() })
        }
        TypeKind::Tuple(elems) => {
            let mut new_elems: Vec<Type> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                new_elems.push(rewrite_placeholders(&elems[i], synth_names, prefix));
                i += 1;
            }
            TypeKind::Tuple(new_elems)
        }
        TypeKind::Ref { inner, mutable, lifetime } => TypeKind::Ref {
            inner: Box::new(rewrite_placeholders(inner, synth_names, prefix)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        TypeKind::RawPtr { inner, mutable } => TypeKind::RawPtr {
            inner: Box::new(rewrite_placeholders(inner, synth_names, prefix)),
            mutable: *mutable,
        },
        TypeKind::Slice(inner) => {
            TypeKind::Slice(Box::new(rewrite_placeholders(inner, synth_names, prefix)))
        }
        TypeKind::SelfType => TypeKind::SelfType,
        TypeKind::Never => TypeKind::Never,
        // ImplTrait can't contain `_` placeholders meaningfully —
        // resolve_type will reject the whole `impl Trait` later if it
        // reaches a non-arg site. Pass through unchanged.
        TypeKind::ImplTrait(b) => TypeKind::ImplTrait(b.clone()),
        TypeKind::FnPtr { params, ret } => {
            let mut new_params: Vec<Type> = Vec::with_capacity(params.len());
            let mut i = 0;
            while i < params.len() {
                new_params.push(rewrite_placeholders(&params[i], synth_names, prefix));
                i += 1;
            }
            let new_ret = ret
                .as_ref()
                .map(|r| Box::new(rewrite_placeholders(r, synth_names, prefix)));
            TypeKind::FnPtr { params: new_params, ret: new_ret }
        }
        // `dyn Trait` doesn't admit `_` placeholders inside its bound
        // list (no syntactic position for one). Pass through unchanged.
        TypeKind::Dyn { bounds, lifetime } => TypeKind::Dyn {
            bounds: bounds.clone(),
            lifetime: lifetime.clone(),
        },
    };
    Type { kind, span: ty.span.copy() }
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
        InferType::Opaque { fn_path, slot } => {
            format!("impl <{}#{}>", place_to_string(fn_path), slot)
        }
        InferType::FnPtr { params, ret } => {
            let mut s = String::from("fn(");
            let mut i = 0;
            while i < params.len() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&infer_to_string(&params[i]));
                i += 1;
            }
            s.push(')');
            if !matches!(ret.as_ref(), InferType::Tuple(v) if v.is_empty()) {
                s.push_str(" -> ");
                s.push_str(&infer_to_string(ret));
            }
            s
        }
        InferType::Dyn { bounds, .. } => {
            let mut s = String::from("dyn ");
            let mut i = 0;
            while i < bounds.len() {
                if i > 0 {
                    s.push_str(" + ");
                }
                s.push_str(&place_to_string(&bounds[i].trait_path));
                i += 1;
            }
            s
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
            InferType::Slice(inner) => InferType::Slice(Box::new(self.substitute(inner))),
            InferType::Str => InferType::Str,
            InferType::AssocProj { base, trait_path, name } => InferType::AssocProj {
                base: Box::new(self.substitute(base)),
                trait_path: trait_path.clone(),
                name: name.clone(),
            },
            InferType::Never => InferType::Never,
            InferType::Char => InferType::Char,
            InferType::Opaque { fn_path, slot } => InferType::Opaque {
                fn_path: fn_path.clone(),
                slot: *slot,
            },
            InferType::FnPtr { params, ret } => InferType::FnPtr {
                params: params.iter().map(|p| self.substitute(p)).collect(),
                ret: Box::new(self.substitute(ret)),
            },
            InferType::Dyn { bounds, lifetime } => InferType::Dyn {
                bounds: bounds.clone(),
                lifetime: lifetime.clone(),
            },
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

    // Coerce — `actual` is an expression's inferred type flowing into a
    // position expecting `expected`. Asymmetric: only the value-level
    // `Never` coerces (an expression of type `!` flows into any
    // expected type without binding it). All other shapes fall through
    // to the invariant `unify`.
    //
    // Use this at every value-flow boundary (block tail vs return,
    // call arg vs param, let RHS vs annotation, …). For pure type/type
    // equation (turbofish, structural recursion, pattern asserts), use
    // `unify` directly.
    pub(crate) fn coerce(
        &mut self,
        actual: &InferType,
        expected: &InferType,
        traits: &TraitTable,
        type_params: &Vec<String>,
        type_param_bounds: &Vec<Vec<Vec<String>>>,
        span: &Span,
        file: &str,
    ) -> Result<(), Error> {
        let resolved_actual = self.substitute(actual);
        if matches!(resolved_actual, InferType::Never) {
            return Ok(());
        }
        self.unify(actual, expected, traits, type_params, type_param_bounds, span, file)
    }

    // Merge — symmetric variant for if/match arm-merge sites where
    // neither side is canonically actual/expected. If either side is
    // `Never`, the other wins (the merged type is the non-Never one).
    // Otherwise the two are unified invariantly and `a` is returned
    // (after unify, both are equivalent). Returns the merged type so
    // the caller can use it as the if/match's overall expression type
    // without a separate post-substitute pick.
    pub(crate) fn merge(
        &mut self,
        a: &InferType,
        b: &InferType,
        traits: &TraitTable,
        type_params: &Vec<String>,
        type_param_bounds: &Vec<Vec<Vec<String>>>,
        span: &Span,
        file: &str,
    ) -> Result<InferType, Error> {
        let ra = self.substitute(a);
        let rb = self.substitute(b);
        if matches!(ra, InferType::Never) {
            return Ok(b.clone());
        }
        if matches!(rb, InferType::Never) {
            return Ok(a.clone());
        }
        self.unify(a, b, traits, type_params, type_param_bounds, span, file)?;
        Ok(a.clone())
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
            // Never's value-level coercion lives in `coerce` / `merge`,
            // not here. Unification is invariant: `!` equals only `!`.
            // This matches Rust's behavior — recursing into a fn-pointer
            // return type (or any structural position) `unify(!, T)`
            // rejects, just as `let _: fn() -> u32 = …: fn() -> !`
            // rejects in real Rust.
            (InferType::Never, InferType::Never) => Ok(()),
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
            (
                InferType::Opaque { fn_path: a_fp, slot: a_slot },
                InferType::Opaque { fn_path: b_fp, slot: b_slot },
            ) if a_fp == b_fp && a_slot == b_slot => Ok(()),
            (
                InferType::Dyn { bounds: ba, .. },
                InferType::Dyn { bounds: bb, .. },
            ) => {
                if infer_dyn_bounds_eq(&ba, &bb) {
                    Ok(())
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            infer_to_string(&InferType::Dyn { bounds: bb.clone(), lifetime: LifetimeRepr::Inferred(0) }),
                            infer_to_string(&InferType::Dyn { bounds: ba.clone(), lifetime: LifetimeRepr::Inferred(0) }),
                        ),
                        span: span.copy(),
                    })
                }
            }
            (
                InferType::FnPtr { params: pa, ret: ra },
                InferType::FnPtr { params: pb, ret: rb },
            ) => {
                if pa.len() != pb.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "fn-ptr arity mismatch: expected {} params, got {}",
                            pb.len(),
                            pa.len()
                        ),
                        span: span.copy(),
                    });
                }
                let mut i = 0;
                while i < pa.len() {
                    self.unify(
                        &pa[i],
                        &pb[i],
                        traits,
                        type_params,
                        type_param_bounds,
                        span,
                        file,
                    )?;
                    i += 1;
                }
                self.unify(ra.as_ref(), rb.as_ref(), traits, type_params, type_param_bounds, span, file)
            }
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
            InferType::Opaque { fn_path, slot } => RType::Opaque { fn_path, slot },
            InferType::FnPtr { params, ret } => RType::FnPtr {
                params: params.iter().map(|p| self.finalize(p)).collect(),
                ret: Box::new(self.finalize(&ret)),
            },
            InferType::Dyn { bounds, lifetime } => RType::Dyn {
                bounds: bounds
                    .iter()
                    .map(|b| DynBound {
                        trait_path: b.trait_path.clone(),
                        trait_args: b.trait_args.iter().map(|t| self.finalize(t)).collect(),
                        assoc_bindings: b
                            .assoc_bindings
                            .iter()
                            .map(|(n, t)| (n.clone(), self.finalize(t)))
                            .collect(),
                    })
                    .collect(),
                lifetime,
            },
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

// Returns true iff `path` is the canonical `std::boxed::Box` path.
// Used to recognize `Box<T>` types at coercion + dispatch sites.
pub fn is_std_box_path(path: &Vec<String>) -> bool {
    path.len() == 3 && path[0] == "std" && path[1] == "boxed" && path[2] == "Box"
}

// Canonicalize a `dyn`-context trait path. `resolve_type`'s Dyn arm
// stored a probe-free best-effort canonicalization (use-scope explicit
// match, otherwise module-relative); that doesn't see glob imports
// like the stdlib prelude (`use std::*;`), so a bare `Fn` written in
// user code stays as `[<user-module>, "Fn"]` which `trait_lookup`
// can't find. Re-canonicalize here with the full `resolve_trait_path`
// machinery, which probes against TraitTable and handles globs +
// reexports. Returns the input unchanged if no resolution succeeds —
// the caller will then surface a clearer "trait not found" error.
pub fn canonicalize_dyn_trait_path(
    raw: &Vec<String>,
    current_module: &Vec<String>,
    traits: &tables::TraitTable,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Vec<String> {
    let p = crate::ast::Path {
        segments: raw
            .iter()
            .map(|s| crate::ast::PathSegment {
                name: s.clone(),
                span: crate::span::Span::new(crate::span::Pos::new(0, 0), crate::span::Pos::new(0, 0)),
                lifetime_args: Vec::new(),
                args: Vec::new(),
            })
            .collect(),
        span: crate::span::Span::new(crate::span::Pos::new(0, 0), crate::span::Pos::new(0, 0)),
    };
    match setup::resolve_trait_path(&p, current_module, traits, use_scope, reexports, file) {
        Ok(canon) => canon,
        Err(_) => raw.clone(),
    }
}

// Coercion of a `&T` / `&mut T` / `Box<T>` value into the matching dyn
// shape. `src_concrete_ty` is the source's concrete `T`; `bounds` is
// the trait-bound list of the target dyn type (each carrying
// `trait_args` + `assoc_bindings`). `kind` discriminates the outer
// container so codegen knows whether to emit a fat reference or a fat
// box. Mono uses the (bound, concrete_ty) pair to request vtable
// construction with the right trait-args; codegen emits the data ptr
// from the source followed by the vtable address as the second word.
#[derive(Clone)]
pub struct DynCoercion {
    pub src_concrete_ty: RType,
    pub bounds: Vec<DynBound>,
    pub kind: DynCoercionKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DynCoercionKind {
    // `&T → &dyn Trait` or `&mut T → &mut dyn Trait` (or downgrade).
    Ref,
    // `Box<T> → Box<dyn Trait>`. Box's runtime layout is a single i32
    // (the heap data ptr) on the source side; the coercion turns it
    // into a (data_ptr, vtable_ptr) fat box.
    BoxOwned,
}

// Dispatch record for a method call whose receiver is `&dyn Trait` /
// `&mut dyn Trait`. Codegen loads vtable[method_idx*4] from the fat
// ref's vtable pointer and emits `call_indirect` with a typeidx built
// from `method_param_types`/`method_return_type` (with Self → erased).
#[derive(Clone)]
pub struct DynMethodDispatch {
    pub trait_path: Vec<String>,
    pub method_idx: u32,
    // The method's signature, with Self substituted to the dyn-ref's
    // shape. Used by codegen to build the call_indirect typeidx.
    pub method_param_types: Vec<RType>,
    pub method_return_type: RType,
    // Receiver shape: `&self` → false, `&mut self` → true. Codegen
    // lowers the receiver fat ref's data ptr as the &self/&mut self
    // arg.
    pub recv_mut: bool,
}

// Coerce `actual` into `expected` at expression `expr_id`. If the
// shape is the unsizing coercion `&T → &dyn Trait` (or `&mut T → &mut
// dyn Trait`), runs object-safety + impl-existence checks and records
// a `DynCoercion` on `ctx.dyn_coercions[expr_id]` so mono lowers the
// expression as a fat-pointer construction. Otherwise falls back to
// the standard invariant `coerce`.
pub(crate) fn coerce_at(
    ctx: &mut CheckCtx,
    expr_id: crate::ast::NodeId,
    actual: &InferType,
    expected: &InferType,
    span: &Span,
) -> Result<(), Error> {
    let actual_resolved = ctx.subst.substitute(actual);
    let expected_resolved = ctx.subst.substitute(expected);
    if let (
        InferType::Ref { inner: a_inner, mutable: am, .. },
        InferType::Ref { inner: e_inner, mutable: em, .. },
    ) = (&actual_resolved, &expected_resolved)
    {
        if let InferType::Dyn { bounds, .. } = e_inner.as_ref() {
            // The source's inner must NOT itself be a Dyn (that would
            // be a no-op identity — `&dyn T → &dyn T` — and should go
            // through normal unify, not trigger a coercion + impl
            // check that would predictably fail).
            if matches!(a_inner.as_ref(), InferType::Dyn { .. }) {
                return ctx.subst.coerce(
                    actual,
                    expected,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    span,
                    ctx.current_file,
                );
            }
            // `&T` coerces to `&dyn Trait`; `&mut T` coerces to either
            // `&mut dyn Trait` (preserving mutability) or `&dyn Trait`
            // (downgrading). Disallow the reverse — `&T` → `&mut dyn`.
            if *em && !*am {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "cannot coerce `&T` to `&mut dyn Trait`: source ref is not mutable".to_string(),
                    span: span.copy(),
                });
            }
            // Canonicalize each bound through the full use-scope
            // resolver (which sees globs + reexports). resolve_type's
            // probe-free pass leaves bare names like `Fn` un-rewritten.
            let canon_bounds: Vec<InferDynBound> = bounds
                .iter()
                .map(|b| InferDynBound {
                    trait_path: canonicalize_dyn_trait_path(
                        &b.trait_path,
                        ctx.current_module,
                        ctx.traits,
                        &ctx.use_scope,
                        ctx.reexports,
                        ctx.current_file,
                    ),
                    trait_args: b.trait_args.clone(),
                    assoc_bindings: b.assoc_bindings.clone(),
                })
                .collect();
            let bounds = canon_bounds;
            // Run object-safety on each bound trait.
            let mut i = 0;
            while i < bounds.len() {
                dyn_safety::check_object_safety(&bounds[i].trait_path, ctx.traits, span, ctx.current_file)?;
                i += 1;
            }
            // Resolve the source's concrete type and verify the bound
            // is implemented for it. Synthesized closure structs
            // (`__closure_N`) get their `Fn`/`FnMut`/`FnOnce` impls
            // registered post-typeck by `closure_lower`, so the impl
            // genuinely doesn't exist at this point — skip the check
            // for closure srcs.
            let src_concrete = ctx.subst.finalize(a_inner);
            let src_is_closure = matches!(
                &src_concrete,
                RType::Struct { path, .. }
                    if path.last().map(|s| s.starts_with("__closure_")).unwrap_or(false)
            );
            let mut j = 0;
            while j < bounds.len() {
                if src_is_closure {
                    j += 1;
                    continue;
                }
                let trait_args_rt: Vec<RType> = bounds[j].trait_args.iter().map(|t| ctx.subst.finalize(t)).collect();
                let solved = traits::solve_impl_with_args(
                    &bounds[j].trait_path,
                    &trait_args_rt,
                    &src_concrete,
                    ctx.traits,
                    0,
                );
                if solved.is_none() {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: format!(
                            "cannot coerce to `&dyn {}`: type `{}` does not implement `{}`",
                            place_to_string(&bounds[j].trait_path),
                            rtype_to_string(&src_concrete),
                            place_to_string(&bounds[j].trait_path),
                        ),
                        span: span.copy(),
                    });
                }
                j += 1;
            }
            // Record. The expr's recorded `expr_infer_types` stays
            // `Ref<Dyn>` (the coerced-to type); codegen reads the
            // matching `DynCoercion` to know how to build the fat ref.
            let id = expr_id as usize;
            if id < ctx.dyn_coercions.len() {
                let bounds_rt: Vec<DynBound> = bounds
                    .iter()
                    .map(|b| DynBound {
                        trait_path: b.trait_path.clone(),
                        trait_args: b.trait_args.iter().map(|t| ctx.subst.finalize(t)).collect(),
                        assoc_bindings: b
                            .assoc_bindings
                            .iter()
                            .map(|(n, t)| (n.clone(), ctx.subst.finalize(t)))
                            .collect(),
                    })
                    .collect();
                ctx.dyn_coercions[id] = Some(DynCoercion {
                    src_concrete_ty: src_concrete,
                    bounds: bounds_rt,
                    kind: DynCoercionKind::Ref,
                });
            }
            return Ok(());
        }
    }
    // Box<T> → Box<dyn Trait>: source = `Struct{["std","boxed","Box"],
    // [T]}`; target = `Struct{["std","boxed","Box"], [Dyn{...}]}`.
    // Same obj-safety + impl-existence checks as the ref case, but the
    // lowered shape is `BoxOwned` (data_ptr field of the source box +
    // vtable address).
    if let (
        InferType::Struct { path: a_path, type_args: a_args, .. },
        InferType::Struct { path: e_path, type_args: e_args, .. },
    ) = (&actual_resolved, &expected_resolved)
    {
        if a_path == e_path
            && is_std_box_path(a_path)
            && a_args.len() == 1
            && e_args.len() == 1
        {
            if let InferType::Dyn { bounds, .. } = &e_args[0] {
                // The source's type-arg must NOT itself be Dyn (that
                // would be a no-op identity, handled by normal unify).
                if !matches!(&a_args[0], InferType::Dyn { .. }) {
                    let canon_bounds: Vec<InferDynBound> = bounds
                        .iter()
                        .map(|b| InferDynBound {
                            trait_path: canonicalize_dyn_trait_path(
                                &b.trait_path,
                                ctx.current_module,
                                ctx.traits,
                                &ctx.use_scope,
                                ctx.reexports,
                                ctx.current_file,
                            ),
                            trait_args: b.trait_args.clone(),
                            assoc_bindings: b.assoc_bindings.clone(),
                        })
                        .collect();
                    let bounds = canon_bounds;
                    let mut i = 0;
                    while i < bounds.len() {
                        dyn_safety::check_object_safety(&bounds[i].trait_path, ctx.traits, span, ctx.current_file)?;
                        i += 1;
                    }
                    let src_concrete = ctx.subst.finalize(&a_args[0]);
                    let src_is_closure = matches!(
                        &src_concrete,
                        RType::Struct { path, .. }
                            if path.last().map(|s| s.starts_with("__closure_")).unwrap_or(false)
                    );
                    let mut j = 0;
                    while j < bounds.len() {
                        if src_is_closure {
                            j += 1;
                            continue;
                        }
                        let trait_args_rt: Vec<RType> = bounds[j].trait_args.iter().map(|t| ctx.subst.finalize(t)).collect();
                        let solved = traits::solve_impl_with_args(
                            &bounds[j].trait_path,
                            &trait_args_rt,
                            &src_concrete,
                            ctx.traits,
                            0,
                        );
                        if solved.is_none() {
                            return Err(Error {
                                file: ctx.current_file.to_string(),
                                message: format!(
                                    "cannot coerce to `Box<dyn {}>`: type `{}` does not implement `{}`",
                                    place_to_string(&bounds[j].trait_path),
                                    rtype_to_string(&src_concrete),
                                    place_to_string(&bounds[j].trait_path),
                                ),
                                span: span.copy(),
                            });
                        }
                        j += 1;
                    }
                    let id = expr_id as usize;
                    if id < ctx.dyn_coercions.len() {
                        let bounds_rt: Vec<DynBound> = bounds
                            .iter()
                            .map(|b| DynBound {
                                trait_path: b.trait_path.clone(),
                                trait_args: b.trait_args.iter().map(|t| ctx.subst.finalize(t)).collect(),
                                assoc_bindings: b
                                    .assoc_bindings
                                    .iter()
                                    .map(|(n, t)| (n.clone(), ctx.subst.finalize(t)))
                                    .collect(),
                            })
                            .collect();
                        ctx.dyn_coercions[id] = Some(DynCoercion {
                            src_concrete_ty: src_concrete,
                            bounds: bounds_rt,
                            kind: DynCoercionKind::BoxOwned,
                        });
                    }
                    return Ok(());
                }
            }
        }
    }
    ctx.subst.coerce(
        actual,
        expected,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        span,
        ctx.current_file,
    )
}

pub(crate) struct LocalEntry {
    name: String,
    ty: InferType,
    mutable: bool,
    // Bound by `let x: T;` (no initializer). The mutability check
    // for assignment-statements treats such bindings as if they were
    // declared `mut` so the deferred initializer assignment goes
    // through (and any subsequent ones too — pocket-rust doesn't
    // enforce Rust's "single first-init for non-mut" rule, accepting
    // a strict superset). Borrowck's move-state lattice still
    // rejects reads before the first assignment.
    declared_uninit: bool,
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
    // Per-NodeId fn-item address. Recorded when a bare-name `Var(name)`
    // expression resolved to a non-generic fn item that's being coerced
    // into an `RType::FnPtr` slot. Stores the FuncTable callee_idx;
    // copied into FnSymbol/Template.fn_item_addrs at end-of-fn.
    pub(crate) fn_item_addrs: Vec<Option<usize>>,
    // Per-NodeId dyn-trait coercion record. Set when a `&T` (or `&mut T`)
    // value flows into a `&dyn Trait` (or `&mut dyn Trait`) slot at one
    // of the explicit `coerce_at` sites (let-stmt RHS, call arg, fn
    // return, struct field). Carries the source's concrete type and
    // the coerced-to trait paths; mono builds the fat pointer +
    // requests vtable construction, codegen emits the data-segment
    // address as the second word of the fat ref. None at every other
    // expr id.
    pub(crate) dyn_coercions: Vec<Option<DynCoercion>>,
    // Per-NodeId dyn-method dispatch. Set on a MethodCall expr id when
    // the receiver was `&dyn Trait` / `&mut dyn Trait`. Codegen reads
    // the vtable index + method signature here to emit `call_indirect`.
    pub(crate) dyn_method_calls: Vec<Option<DynMethodDispatch>>,
    // Per-NodeId resolved RType type-args for builtins that need them at
    // codegen (`¤size_of::<T>()`). `None` outside builtin-with-types
    // sites. Finalized into FnSymbol.builtin_type_targets at end-of-fn.
    pub(crate) builtin_type_targets: Vec<Option<Vec<RType>>>,
    // Per-pattern.id ergonomics record (sized to func.node_count).
    // Default-zero means "no auto-peel/binding-override at this pattern
    // node". `check_pattern` writes here when it traverses ref scrutinees
    // with non-ref patterns or applies a non-Move default binding mode.
    pub(crate) pattern_ergo: Vec<PatternErgo>,
    // Per-NodeId resolved const value: when a `Var(name)` resolved to
    // a const item, the const's value is denormalized here. Codegen
    // reads at use sites and emits a literal. `None` for locals /
    // unresolved Vars (the latter being typeck errors).
    pub(crate) const_uses: Vec<Option<ConstValue>>,
    pub(crate) subst: Subst,
    pub(crate) current_module: &'a Vec<String>,
    pub(crate) current_crate: &'a str,
    pub(crate) current_file: &'a str,
    pub(crate) structs: &'a StructTable,
    pub(crate) enums: &'a EnumTable,
    pub(crate) aliases: &'a AliasTable,
    pub(crate) traits: &'a TraitTable,
    pub(crate) funcs: &'a mut FuncTable,
    pub(crate) consts: &'a ConstTable,
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
    // Per-type-param positional trait-args parallel to `type_param_bounds`.
    // `[i][j][k]` is the k-th positional arg of the j-th bound on
    // `type_params[i]`. Used by `lookup_fn_bound_signature` to extract
    // the `(P0, P1, …)` tuple from `T: Fn(P0, P1) -> R` bounds when
    // dispatching bare `f(args)` against a type-param-typed local.
    pub(crate) type_param_bound_args: &'a Vec<Vec<Vec<RType>>>,
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
    // Per-NodeId pending closure record (sized to func.node_count).
    // `Some(_)` at each `ExprKind::Closure` site, `None` elsewhere.
    // Holds InferTypes during body check; finalized into RType-bearing
    // `ClosureInfo` and stored on FnSymbol/GenericTemplate.closures at
    // end-of-fn.
    pub(crate) closure_records: Vec<Option<PendingClosure>>,
    // Stack of enclosing closure scopes (innermost-last). Each frame
    // records the locals-stack length captured when the closure was
    // entered (the "capture barrier") plus the synthesized struct path
    // assigned to that closure. Var lookups consult the innermost
    // frame: if the resolved local has idx `< local_barrier`, the
    // lookup crosses the barrier and is treated as a capture (rejected
    // in phase 1; recorded in phase 2).
    pub(crate) closure_scopes: Vec<ClosureScope>,
    // Bidirectional inference: when a closure expression is a call
    // argument and the corresponding parameter has a `Fn(A) -> R`
    // bound, the call-site stashes (param_types, return_type) here
    // keyed by the closure's NodeId. `check_closure` consumes the
    // entry on entry, using the stashed types as expected_signature
    // (instead of fresh inference vars) for params/return. Lifted by
    // `check_closure` after consumption.
    pub(crate) expected_closure_signatures: Vec<Option<(Vec<InferType>, InferType)>>,
    // Per-NodeId bare-closure-call records. `Some(binding_name)` when
    // the call's callee resolved to a local of closure type;
    // finalized into FnSymbol/Template's `bare_closure_calls` so mono
    // can lower these as MethodCall MonoExprs without AST mutation.
    pub(crate) bare_closure_calls: Vec<Option<String>>,
}

pub(crate) struct ClosureScope {
    pub local_barrier: usize,
    pub node_id: u32,
    pub synthesized_struct_path: Vec<String>,
    // Bindings the body referenced from outside `local_barrier`.
    // Captures are deduplicated by name and recorded in first-reference
    // order — the lowering pass uses this order to lay out struct
    // fields and to populate the struct literal at the closure
    // expression site.
    pub captures: Vec<PendingCapture>,
}

#[derive(Clone)]
pub(crate) struct PendingCapture {
    pub binding_name: String,
    pub captured_ty: InferType,
    // Set when typeck observes any mutation of the captured binding
    // inside the closure body (assignment, compound-assign,
    // `&mut`-borrow). Drives capture-mode upgrade `Ref → RefMut` and
    // makes lowering skip the `Fn` impl (only FnMut + FnOnce
    // synthesized).
    pub mutated: bool,
}

#[derive(Clone)]
pub(crate) struct PendingClosure {
    pub synthesized_struct_path: Vec<String>,
    pub param_types: Vec<InferType>,
    pub return_type: InferType,
    pub is_move: bool,
    pub body_span: Span,
    pub captures: Vec<PendingCapture>,
    // Snapshot of the enclosing fn's type-params at closure-checking
    // time — propagated through to ClosureInfo so synthesis can build
    // a generic struct + impl. See `ClosureInfo.enclosing_type_params`.
    pub enclosing_type_params: Vec<String>,
}

fn check_module(
    module: &Module,
    path: &mut Vec<String>,
    root_crate_name: &str,
    current_file: &mut String,
    structs: &StructTable,
    enums: &EnumTable,
    aliases: &AliasTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
    consts: &ConstTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let crate_root: &str = root_crate_name;
    let use_scope = module_use_entries(module, crate_root);
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                check_function(f, path, root_crate_name, path, None, current_file, structs, enums, aliases, traits, funcs, consts, reexports, &use_scope)?
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                check_module(m, path, root_crate_name, current_file, structs, enums, aliases, traits, funcs, consts, reexports)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(ib) => {
                let target_rt = resolve_impl_target(ib, path, structs, enums, aliases, &use_scope, reexports, current_file)?;
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
                        root_crate_name,
                        &method_prefix,
                        Some(&target_rt),
                        current_file,
                        structs,
                        enums,
                        aliases,
                        traits,
                        funcs,
                        consts,
                        reexports,
                        &use_scope,
                    )?;
                    k += 1;
                }
            }
            Item::Trait(_) => {}
            Item::Use(_) => {}
            Item::TypeAlias(_) => {}
            Item::Const(_) => {}
        }
        i += 1;
    }
    *current_file = saved;
    Ok(())
}

fn check_function(
    func: &Function,
    current_module: &Vec<String>,
    current_crate: &str,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    current_file: &str,
    structs: &StructTable,
    enums: &EnumTable,
    aliases: &AliasTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
    consts: &ConstTable,
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
    let (type_param_names, type_param_bounds, type_param_bound_args): (
        Vec<String>,
        Vec<Vec<Vec<String>>>,
        Vec<Vec<Vec<RType>>>,
    ) = match template_lookup(funcs, &lookup_path) {
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
            (
                t.type_params.clone(),
                bounds_clone,
                t.type_param_bound_args.clone(),
            )
        }
        None => (Vec::new(), Vec::new(), Vec::new()),
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
                        aliases,
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
    // Build initial locals from params. `fn f(mut x: T)` makes the
    // parameter binding mutable so the body can re-assign `x`.
    let mut locals: Vec<LocalEntry> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let rt = resolve_type(
            &func.params[k].ty,
            current_module,
            structs,
            enums,
            aliases,
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
            mutable: func.params[k].mutable,
            declared_uninit: false,
        });
        k += 1;
    }
    // Reuse the return type that setup already resolved (it ran the
    // RPIT rewrite + concretize pass). Re-resolving from the AST
    // would re-error on `impl Trait` in return position. Look up the
    // FnSymbol/Template by `lookup_path`; missing means no signature
    // was registered (shouldn't happen for a well-formed call into
    // `check_function`).
    let return_rt: Option<RType> = if let Some(e) = func_lookup(funcs, &lookup_path) {
        e.return_type.clone()
    } else if let Some((_, t)) = template_lookup(funcs, &lookup_path) {
        t.return_type.clone()
    } else {
        None
    };

    let node_count = func.node_count as usize;
    let (expr_infer_types, lit_constraints, method_resolutions, call_resolutions, fn_item_addrs, dyn_coercions, dyn_method_calls, builtin_type_targets, pattern_ergo, closure_records, bare_closure_calls, const_uses, subst) = {
        let mut method_res: Vec<Option<PendingMethodCall>> = Vec::with_capacity(node_count);
        let mut call_res: Vec<Option<PendingCall>> = Vec::with_capacity(node_count);
        let mut expr_infer: Vec<Option<InferType>> = Vec::with_capacity(node_count);
        let mut btt: Vec<Option<Vec<RType>>> = Vec::with_capacity(node_count);
        let mut pat_ergo: Vec<PatternErgo> = Vec::with_capacity(node_count);
        let mut const_uses_init: Vec<Option<ConstValue>> = Vec::with_capacity(node_count);
        let mut fn_item_addrs_init: Vec<Option<usize>> = Vec::with_capacity(node_count);
        let mut dyn_coercions_init: Vec<Option<DynCoercion>> = Vec::with_capacity(node_count);
        let mut dyn_method_calls_init: Vec<Option<DynMethodDispatch>> = Vec::with_capacity(node_count);
        let mut i = 0;
        while i < node_count {
            method_res.push(None);
            call_res.push(None);
            expr_infer.push(None);
            btt.push(None);
            pat_ergo.push(PatternErgo::default());
            const_uses_init.push(None);
            fn_item_addrs_init.push(None);
            dyn_coercions_init.push(None);
            dyn_method_calls_init.push(None);
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
            fn_item_addrs: fn_item_addrs_init,
            dyn_coercions: dyn_coercions_init,
            dyn_method_calls: dyn_method_calls_init,
            builtin_type_targets: btt,
            pattern_ergo: pat_ergo,
            const_uses: const_uses_init,
            subst: Subst {
                bindings: Vec::new(),
                is_num_lit: Vec::new(),
            },
            current_module,
            current_crate,
            current_file,
            structs,
            enums,
            aliases,
            traits,
            funcs: &mut *funcs,
            consts,
            self_target,
            type_params: &type_param_names,
            type_param_bounds: &type_param_bounds,
            type_param_bound_args: &type_param_bound_args,
            type_param_bound_assoc: &type_param_bound_assoc,
            reexports,
            use_scope: initial_use_scope,
            loop_labels: Vec::new(),
            fn_return_rt: return_rt.clone(),
            closure_records: vec![None; func.node_count as usize],
            closure_scopes: Vec::new(),
            expected_closure_signatures: vec![None; func.node_count as usize],
            bare_closure_calls: vec![None; func.node_count as usize],
        };
        check_block(&mut ctx, &func.body, &return_rt)?;
        (
            ctx.expr_infer_types,
            ctx.lit_constraints,
            ctx.method_resolutions,
            ctx.call_resolutions,
            ctx.fn_item_addrs,
            ctx.dyn_coercions,
            ctx.dyn_method_calls,
            ctx.builtin_type_targets,
            ctx.pattern_ergo,
            ctx.closure_records,
            ctx.bare_closure_calls,
            ctx.const_uses,
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
                        // Skip the impl-existence check when the
                        // receiver is a synthesized closure struct: the
                        // matching `Fn`/`FnMut`/`FnOnce` impl is
                        // registered by `closure_lower` after typeck
                        // finishes, so the impl genuinely doesn't exist
                        // yet at this point. Codegen calls
                        // `solve_impl_with_args` later (with the impl
                        // registered) to pick the row.
                        let recv_is_closure = match &recv_type {
                            RType::Struct { path, .. } => path
                                .last()
                                .map(|s| s.starts_with("__closure_"))
                                .unwrap_or(false),
                            RType::Ref { inner, .. } => match inner.as_ref() {
                                RType::Struct { path, .. } => path
                                    .last()
                                    .map(|s| s.starts_with("__closure_"))
                                    .unwrap_or(false),
                                _ => false,
                            },
                            _ => false,
                        };
                        // Opaque receiver — the slot's bounds (already
                        // checked at body time) guarantee the trait
                        // is satisfied by the eventual concrete pin.
                        // After post-typeck `finalize_rpit_substitutions`
                        // codegen sees the concrete type and re-resolves
                        // the impl normally; here at typeck no real
                        // impl exists for `Opaque`, so skip.
                        let recv_is_opaque = match &recv_type {
                            RType::Opaque { .. } => true,
                            RType::Ref { inner, .. } => {
                                matches!(inner.as_ref(), RType::Opaque { .. })
                            }
                            _ => false,
                        };
                        if needs_validation && !recv_is_closure && !recv_is_opaque {
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
            Some(PendingCall::Indirect { callee_local_name, param_infers, ret_infer }) => {
                let mut params: Vec<RType> = Vec::with_capacity(param_infers.len());
                let mut j = 0;
                while j < param_infers.len() {
                    params.push(subst.finalize(&param_infers[j]));
                    j += 1;
                }
                let ret = subst.finalize(ret_infer);
                call_resolutions_final.push(Some(CallResolution::Indirect {
                    callee_local_name: callee_local_name.clone(),
                    fn_ptr_ty: RType::FnPtr {
                        params,
                        ret: Box::new(ret),
                    },
                }));
            }
            None => call_resolutions_final.push(None),
        }
        i += 1;
    }
    let call_resolutions = call_resolutions_final;

    // Finalize per-closure records (per-NodeId).
    let mut closures_final: Vec<Option<ClosureInfo>> = Vec::with_capacity(node_count);
    let mut i = 0;
    while i < closure_records.len() {
        match &closure_records[i] {
            Some(pc) => {
                let mut param_types: Vec<RType> = Vec::new();
                let mut k = 0;
                while k < pc.param_types.len() {
                    param_types.push(subst.finalize(&pc.param_types[k]));
                    k += 1;
                }
                let return_raw = subst.finalize(&pc.return_type);
                // The body type may end up as an `AssocProj` (e.g.
                // `<Self as Add>::Output` for an unannotated `|x| x + 1`
                // closure where dispatch went through the symbolic
                // num-lit path). Concretize against the *enclosing*
                // function's bounds so `Self::Output` collapses to
                // `Self` for the operator traits whose `Output =
                // Self` invariant `assoc_always_equals_self`
                // recognizes — gives us the resolved integer kind
                // (defaulting to i32) for closure return types.
                let return_type = concretize_assoc_proj_with_bounds(
                    &return_raw,
                    traits,
                    &Vec::new(),
                    &Vec::new(),
                );
                let mut captures: Vec<CaptureInfo> = Vec::new();
                let body_mutates_capture =
                    pc.captures.iter().any(|cap| cap.mutated);
                let mut c = 0;
                while c < pc.captures.len() {
                    let captured_ty = subst.finalize(&pc.captures[c].captured_ty);
                    // Capture mode:
                    //   `move`     → Move (owned in struct)
                    //   mutated    → RefMut (`&'cap mut T`) — even
                    //                for Copy, so mutations write
                    //                back to the outer binding
                    //                (matches Rust's `|| x += 1`
                    //                capture-by-mut-ref semantics).
                    //   read-only  → Move (Copy) | Ref (non-Copy)
                    let captured_is_copy = is_copy(&captured_ty, traits);
                    let mode = if pc.is_move {
                        CaptureMode::Move
                    } else if pc.captures[c].mutated {
                        CaptureMode::RefMut
                    } else if captured_is_copy {
                        CaptureMode::Move
                    } else {
                        CaptureMode::Ref
                    };
                    captures.push(CaptureInfo {
                        binding_name: pc.captures[c].binding_name.clone(),
                        captured_ty,
                        mode,
                    });
                    c += 1;
                }
                closures_final.push(Some(ClosureInfo {
                    synthesized_struct_path: pc.synthesized_struct_path.clone(),
                    param_types,
                    return_type,
                    is_move: pc.is_move,
                    captures,
                    body_span: pc.body_span.copy(),
                    source_file: current_file.to_string(),
                    body_mutates_capture,
                    enclosing_type_params: pc.enclosing_type_params.clone(),
                }));
            }
            None => closures_final.push(None),
        }
        i += 1;
    }
    let closures = closures_final;

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
        funcs.entries[e].const_uses = const_uses;
        funcs.entries[e].method_resolutions = method_resolutions;
        funcs.entries[e].call_resolutions = call_resolutions;
        funcs.entries[e].fn_item_addrs = fn_item_addrs;
        funcs.entries[e].dyn_coercions = dyn_coercions;
        funcs.entries[e].dyn_method_calls = dyn_method_calls;
        funcs.entries[e].builtin_type_targets = builtin_type_targets;
        funcs.entries[e].pattern_ergo = pattern_ergo;
        funcs.entries[e].closures = closures;
        funcs.entries[e].bare_closure_calls = bare_closure_calls;
    } else {
        let mut t = 0;
        while t < funcs.templates.len() {
            if funcs.templates[t].path == full {
                funcs.templates[t].expr_types = expr_types;
                funcs.templates[t].const_uses = const_uses;
                funcs.templates[t].method_resolutions = method_resolutions;
                funcs.templates[t].call_resolutions = call_resolutions;
                funcs.templates[t].fn_item_addrs = fn_item_addrs;
                funcs.templates[t].dyn_coercions = dyn_coercions;
                funcs.templates[t].dyn_method_calls = dyn_method_calls;
                funcs.templates[t].builtin_type_targets = builtin_type_targets;
                funcs.templates[t].pattern_ergo = pattern_ergo;
                funcs.templates[t].closures = closures;
                funcs.templates[t].bare_closure_calls = bare_closure_calls;
                break;
            }
            t += 1;
        }
    }
    // RPIT slots were pinned in `check_block_with_expected`. No
    // per-fn finalize substitution runs — `Opaque{this_fn, slot}` stays
    // as a stable indirection in tables. Callers in this crate that
    // method-dispatch on the Opaque receiver route through
    // `check_method_call_opaque` (slot-bounds-based dispatch), which
    // works whether or not the slot's pin is set. Consumers outside
    // typeck call `peel_opaque` to materialize the concrete type.
    let _ = lookup_path;
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
    // `f(args)` where `f` is a local of `fn(...) -> R` type. Records
    // the local's name (codegen recovers the storage via local lookup)
    // and the FnPtr's signature as InferTypes (finalized at end-of-fn
    // and lowered to `CallResolution::Indirect` with concrete RTypes).
    Indirect {
        callee_local_name: String,
        param_infers: Vec<InferType>,
        ret_infer: InferType,
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
    // RPIT path: each `Opaque { fn_path, slot }` in the declared
    // return becomes a fresh inference var so unification pins it
    // from the body's actual return shape. After unify, each var
    // resolves to the concrete sub-type — that's the slot's pin.
    // Validate the pin against the slot's bounds before storing.
    let mut opaque_slots: Vec<(u32, Vec<String>, u32)> = Vec::new();
    let expected_infer = rtype_to_infer_with_opaque_vars(
        &expected,
        &mut ctx.subst,
        &mut opaque_slots,
    );
    let span = tail_span_or_block(block);
    // Use the tail expression's node id (or, for tail-less blocks, the
    // block's last-stmt id approximated by 0 — there's nothing to
    // coerce-from anyway). Allows `&T → &dyn Trait` coercion at the
    // function's tail-return site.
    let tail_id = match &block.tail {
        Some(t) => t.id,
        None => 0,
    };
    coerce_at(ctx, tail_id, &actual, &expected_infer, &span)?;
    // Per-Opaque post-unify pinning + bound validation.
    let mut k = 0;
    while k < opaque_slots.len() {
        let (var, fn_path, slot) = opaque_slots[k].clone();
        let resolved = ctx.subst.substitute(&InferType::Var(var));
        // Unbound Var ⇒ the body produced no concrete type that
        // pinned this slot. Cases: diverging body (Never coerces at
        // the body-tail site without binding the slot's Var), zero-
        // iteration loop, or any future "no value flows" case. Real
        // Rust accepts these — the function never returns, so any
        // trait obligation on the slot is vacuously true. Pin to `!`
        // so downstream consumers (`byte_size_of`, layout) see a
        // concrete zero-sized type rather than an unresolved Var.
        if matches!(resolved, InferType::Var(_)) {
            record_rpit_pin(ctx.funcs, &fn_path, slot, RType::Never);
            k += 1;
            continue;
        }
        let pinned_rt = infer_to_rtype_for_check(&resolved);
        // Look up the slot's bounds from FuncTable. Try entries
        // first, then templates.
        let bounds = lookup_rpit_slot_bounds(ctx.funcs, &fn_path, slot);
        let bounds = match bounds {
            Some(b) => b,
            None => {
                k += 1;
                continue;
            }
        };
        let mut bi = 0;
        while bi < bounds.len() {
            let b = &bounds[bi];
            if traits::solve_impl_in_ctx_with_args(
                &b.trait_path,
                &b.trait_args,
                &pinned_rt,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bounds,
                0,
            )
            .is_none()
            {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "RPIT body return type `{}` does not satisfy bound `{}` declared on `{}`",
                        rtype_to_string(&pinned_rt),
                        place_to_string(&b.trait_path),
                        place_to_string(&fn_path),
                    ),
                    span: span.copy(),
                });
            }
            bi += 1;
        }
        // Pin the slot.
        record_rpit_pin(ctx.funcs, &fn_path, slot, pinned_rt);
        k += 1;
    }
    Ok(())
}

// Convert an RType to an InferType, allocating a fresh inference
// variable at each `Opaque` slot. `out` collects `(var, fn_path,
// slot)` triples so the caller can post-unify look up the resolved
// concrete type and pin the slot.
fn rtype_to_infer_with_opaque_vars(
    rt: &RType,
    subst: &mut Subst,
    out: &mut Vec<(u32, Vec<String>, u32)>,
) -> InferType {
    match rt {
        RType::Opaque { fn_path, slot } => {
            let v = subst.fresh_var();
            out.push((v, fn_path.clone(), *slot));
            InferType::Var(v)
        }
        RType::Int(k) => InferType::Int(*k),
        RType::Bool => InferType::Bool,
        RType::Char => InferType::Char,
        RType::Str => InferType::Str,
        RType::Never => InferType::Never,
        RType::Param(name) => InferType::Param(name.clone()),
        RType::Struct { path, type_args, lifetime_args } => InferType::Struct {
            path: path.clone(),
            type_args: type_args
                .iter()
                .map(|a| rtype_to_infer_with_opaque_vars(a, subst, out))
                .collect(),
            lifetime_args: lifetime_args.clone(),
        },
        RType::Enum { path, type_args, lifetime_args } => InferType::Enum {
            path: path.clone(),
            type_args: type_args
                .iter()
                .map(|a| rtype_to_infer_with_opaque_vars(a, subst, out))
                .collect(),
            lifetime_args: lifetime_args.clone(),
        },
        RType::Tuple(elems) => InferType::Tuple(
            elems
                .iter()
                .map(|e| rtype_to_infer_with_opaque_vars(e, subst, out))
                .collect(),
        ),
        RType::Ref { inner, mutable, lifetime } => InferType::Ref {
            inner: Box::new(rtype_to_infer_with_opaque_vars(inner, subst, out)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(rtype_to_infer_with_opaque_vars(inner, subst, out)),
            mutable: *mutable,
        },
        RType::Slice(inner) => InferType::Slice(Box::new(
            rtype_to_infer_with_opaque_vars(inner, subst, out),
        )),
        RType::AssocProj { base, trait_path, name } => InferType::AssocProj {
            base: Box::new(rtype_to_infer_with_opaque_vars(base, subst, out)),
            trait_path: trait_path.clone(),
            name: name.clone(),
        },
        RType::FnPtr { params, ret } => InferType::FnPtr {
            params: params
                .iter()
                .map(|p| rtype_to_infer_with_opaque_vars(p, subst, out))
                .collect(),
            ret: Box::new(rtype_to_infer_with_opaque_vars(ret, subst, out)),
        },
        RType::Dyn { bounds, lifetime } => InferType::Dyn {
            bounds: bounds.iter().map(rtype_dyn_bound_to_infer).collect(),
            lifetime: lifetime.clone(),
        },
    }
}

fn lookup_rpit_slot_bounds(
    funcs: &FuncTable,
    fn_path: &Vec<String>,
    slot: u32,
) -> Option<Vec<crate::typeck::tables::RpitBound>> {
    let mut i = 0;
    while i < funcs.entries.len() {
        if funcs.entries[i].path == *fn_path {
            return funcs.entries[i]
                .rpit_slots
                .get(slot as usize)
                .map(|s| s.bounds.clone());
        }
        i += 1;
    }
    let mut i = 0;
    while i < funcs.templates.len() {
        if funcs.templates[i].path == *fn_path {
            return funcs.templates[i]
                .rpit_slots
                .get(slot as usize)
                .map(|s| s.bounds.clone());
        }
        i += 1;
    }
    None
}

fn record_rpit_pin(
    funcs: &mut FuncTable,
    fn_path: &Vec<String>,
    slot: u32,
    pinned: RType,
) {
    let mut i = 0;
    while i < funcs.entries.len() {
        if funcs.entries[i].path == *fn_path {
            if let Some(s) = funcs.entries[i].rpit_slots.get_mut(slot as usize) {
                s.pin = Some(pinned);
            }
            return;
        }
        i += 1;
    }
    let mut i = 0;
    while i < funcs.templates.len() {
        if funcs.templates[i].path == *fn_path {
            if let Some(s) = funcs.templates[i].rpit_slots.get_mut(slot as usize) {
                s.pin = Some(pinned);
            }
            return;
        }
        i += 1;
    }
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
                flatten_use_tree(&Vec::new(), &decl.tree, crate_root, decl.vis.is_pub_form(), &mut ctx.use_scope);
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
    // `let x: T;` / `let x;` (no initializer): require a single
    // `Binding` pattern (destructure / wildcard / refutable patterns
    // need a value to drive the match), and forbid let-else (there's
    // nothing to test). The annotation is optional — when absent we
    // seed the binding's type with a fresh inference variable, which
    // a later assignment/use can pin via unification. Borrowck seeds
    // the binding as Uninit so reads before the first assignment
    // error.
    if let_stmt.value.is_none() {
        if let_stmt.else_block.is_some() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "uninitialized `let` cannot have an `else` block".to_string(),
                span: let_stmt.pattern.span.copy(),
            });
        }
        if crate::ast::let_simple_binding(let_stmt).is_none() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "uninitialized `let` requires a single binding pattern (destructuring / wildcard / refutable patterns need an initializer)".to_string(),
                span: let_stmt.pattern.span.copy(),
            });
        }
        let binding_infer = match &let_stmt.ty {
            Some(annotation) => {
                let annot_rt = resolve_type(
                    annotation,
                    ctx.current_module,
                    ctx.structs,
                    ctx.enums,
                    ctx.aliases,
                    ctx.self_target,
                    ctx.type_params,
                    &ctx.use_scope,
                    ctx.reexports,
                    ctx.current_file,
                )?;
                rtype_to_infer(&annot_rt)
            }
            None => InferType::Var(ctx.subst.fresh_var()),
        };
        // Reuse the pattern path so the binding lands in locals AND
        // its resolved type is recorded under pattern.id (mono reads
        // it from `expr_types[pat.id]` to allocate the binding's
        // storage at lowering time).
        let mut bindings: Vec<(String, InferType, Span, bool)> = Vec::new();
        check_pattern(ctx, &let_stmt.pattern, &binding_infer, &mut bindings)?;
        let mut k = 0;
        while k < bindings.len() {
            ctx.locals.push(LocalEntry {
                name: bindings[k].0.clone(),
                ty: bindings[k].1.clone(),
                mutable: bindings[k].3,
                declared_uninit: true,
            });
            k += 1;
        }
        return Ok(());
    }
    let value_expr = let_stmt.value.as_ref().expect("just checked is_some");
    let value_ty = check_expr(ctx, value_expr)?;
    let final_ty = match &let_stmt.ty {
        Some(annotation) => {
            // `let x: T = …;` — annotation may contain `_`
            // placeholders (`let x: Vec<_> = …`); use the
            // inference-aware resolver so each placeholder becomes a
            // fresh `InferType::Var` that the unification step pins
            // from the value's type.
            let annot_infer = resolve_type_to_infer(ctx, annotation)?;
            coerce_at(ctx, value_expr.id, &value_ty, &annot_infer, &value_expr.span)?;
            annot_infer
        }
        None => value_ty,
    };
    // Overwrite the recorded type at the value expr's id with the
    // final type (in case an annotation pinned it down). Codegen
    // reads expr_types[value.id] to size the binding's storage.
    ctx.expr_infer_types[value_expr.id as usize] = Some(final_ty.clone());
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
            declared_uninit: false,
        });
        k += 1;
    }
    Ok(())
}


// Upgrade any matching capture entry on enclosing closure scopes to
// `RefMut`. Called when typeck observes a mutation of `binding_name`
// at the local-stack idx `binding_idx`. If the binding is below an
// enclosing closure's capture barrier, that closure is now known to
// mutate the capture — bumps mode `Ref` → `RefMut` (and synthesis
// will skip the `Fn` impl).
// Find the root binding name of a place-shaped expression. Walks
// through `FieldAccess`, `TupleIndex`, and `Deref` chains to the
// innermost `Var(name)`. Returns `None` for anything else (call
// results, struct lits, …) — those can't be place expressions in
// pocket-rust, so a borrow over them isn't a binding-mutation
// observation.
fn root_var_name_of_place(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Var(name) => Some(name.as_str()),
        ExprKind::FieldAccess(fa) => root_var_name_of_place(&fa.base),
        ExprKind::TupleIndex { base, .. } => root_var_name_of_place(base),
        ExprKind::Deref(inner) => root_var_name_of_place(inner),
        _ => None,
    }
}

pub(crate) fn upgrade_capture_to_ref_mut(
    ctx: &mut CheckCtx,
    binding_name: &str,
    binding_idx: usize,
) {
    let binding_ty = ctx.locals[binding_idx].ty.clone();
    let mut sc = 0;
    while sc < ctx.closure_scopes.len() {
        if ctx.closure_scopes[sc].local_barrier > binding_idx {
            let scope = &mut ctx.closure_scopes[sc];
            let mut found = false;
            let mut k = 0;
            while k < scope.captures.len() {
                if scope.captures[k].binding_name == binding_name {
                    scope.captures[k].mutated = true;
                    found = true;
                    break;
                }
                k += 1;
            }
            // Mutating use as the LHS of an assignment is the
            // *first* observation of the capture in
            // `check_assign_stmt` (the Var-lookup-via-rhs hasn't run
            // yet at this point in the assignment-statement check).
            // Record the capture with `mutated: true` so the
            // finalize step picks `RefMut` mode.
            if !found {
                scope.captures.push(PendingCapture {
                    binding_name: binding_name.to_string(),
                    captured_ty: binding_ty.clone(),
                    mutated: true,
                });
            }
        }
        sc += 1;
    }
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
        ctx.subst.coerce(
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
    // Closure capture-mode upgrade: assignment to a captured binding
    // marks the capture as `RefMut` (so the synthesized struct's
    // field type becomes `&'cap mut T` and the closure no longer
    // qualifies for `Fn` — only `FnMut` + `FnOnce` get synthesized).
    upgrade_capture_to_ref_mut(ctx, &chain[0], idx);
    let root_resolved = ctx.subst.substitute(&ctx.locals[idx].ty);
    let root_is_mut_ref = matches!(root_resolved, InferType::Ref { mutable: true, .. });
    let root_is_shared_ref = matches!(root_resolved, InferType::Ref { mutable: false, .. });
    if chain.len() == 1 {
        // Bindings declared via `let x: T;` (uninitialized) accept
        // an assignment without `mut`: the first assignment is an
        // initialization. Pocket-rust's mut-check stays simple — it
        // doesn't enforce Rust's "exactly-one assign for non-mut",
        // accepting a strict superset (the borrowck move-state
        // lattice rejects reads-before-init either way).
        if !ctx.locals[idx].mutable && !ctx.locals[idx].declared_uninit {
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
        ctx.aliases,
        ctx.current_file,
        &assign.lhs.span,
    )?;
    let rhs_ty = check_expr(ctx, &assign.rhs)?;
    ctx.subst.coerce(
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
    // or `&T`, matching Rust.) Use place-mode typing on the root so a
    // FieldAccess root with a non-Copy ref-typed field (e.g. a closure
    // struct's `&mut u32` capture field accessed via `*self.counter =
    // ...`) doesn't trip the value-position move-out-of-borrow check.
    let root_infer = check_place_expr(ctx, root_expr)?;
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
    ctx.subst.coerce(
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
    aliases: &AliasTable,
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

// Closure-capture detection. Called from every binding-resolution site
// (value-position `Var`, place-position `Var`) so the rules stay in
// one place. For each enclosing closure scope whose `local_barrier`
// sits above `binding_idx`, push a `PendingCapture` (deduplicated by
// name; first-reference order preserved). Capture mode (Copy by-value
// vs Ref via `&'cap T`) is decided at end-of-fn finalize.
fn record_capture_if_needed(ctx: &mut CheckCtx, name: &str, binding_idx: usize) {
    let binding_ty = ctx.locals[binding_idx].ty.clone();
    let mut sc = 0;
    while sc < ctx.closure_scopes.len() {
        if ctx.closure_scopes[sc].local_barrier > binding_idx {
            let scope = &mut ctx.closure_scopes[sc];
            let already = scope.captures.iter().any(|c| c.binding_name == name);
            if !already {
                scope.captures.push(PendingCapture {
                    binding_name: name.to_string(),
                    captured_ty: binding_ty.clone(),
                    mutated: false,
                });
            }
        }
        sc += 1;
    }
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
                    let binding_ty = ctx.locals[i].ty.clone();
                    // Closure capture detection — records into every
                    // enclosing closure scope whose barrier sits above
                    // this binding's local idx. Place-position Var
                    // lookups (`check_place_inner::Var`) call the same
                    // helper so capture recording is uniform across
                    // value and place positions.
                    record_capture_if_needed(ctx, name, i);
                    return Ok(binding_ty);
                }
            }
            // Const fallback: not a local — try resolving the name as
            // a `const NAME: T = …;` item via the use_scope. If found,
            // record the const's path on `const_uses[id]` for codegen
            // and yield the const's type as the expression's type.
            let single = vec![name.clone()];
            let resolved = crate::typeck::use_scope::resolve_via_use_scopes(
                &single,
                &ctx.use_scope,
                |cand| const_lookup(ctx.consts, cand).is_some(),
            )
            .or_else(|| {
                // Fall back to `<current_module>::name`.
                let mut full = ctx.current_module.clone();
                full.push(name.clone());
                if const_lookup(ctx.consts, &full).is_some() {
                    Some(full)
                } else {
                    None
                }
            });
            if let Some(path) = resolved {
                let entry = const_lookup(ctx.consts, &path)
                    .expect("just verified above");
                let ty = rtype_to_infer(&entry.ty);
                let id = expr.id as usize;
                if id < ctx.const_uses.len() {
                    ctx.const_uses[id] = Some(entry.value.clone());
                }
                return Ok(ty);
            }
            // Fn-item fallback: a bare path that resolves to a non-
            // generic fn item is an fn-pointer value at this expression
            // position. Generic fn items aren't supported as fn-pointer
            // addresses today (would need higher-order type-arg
            // threading); rejected with a directed error.
            let single_fn = vec![name.clone()];
            let fn_path = crate::typeck::use_scope::resolve_via_use_scopes(
                &single_fn,
                &ctx.use_scope,
                |cand| func_lookup(ctx.funcs, cand).is_some()
                    || template_lookup(ctx.funcs, cand).is_some(),
            )
            .or_else(|| {
                let mut full = ctx.current_module.clone();
                full.push(name.clone());
                if func_lookup(ctx.funcs, &full).is_some()
                    || template_lookup(ctx.funcs, &full).is_some()
                {
                    Some(full)
                } else {
                    None
                }
            });
            if let Some(path) = fn_path {
                if let Some(entry) = func_lookup(ctx.funcs, &path) {
                    let mut callee_idx: Option<usize> = None;
                    let mut k = 0;
                    while k < ctx.funcs.entries.len() {
                        if ctx.funcs.entries[k].path == path {
                            callee_idx = Some(k);
                            break;
                        }
                        k += 1;
                    }
                    let ret = entry
                        .return_type
                        .clone()
                        .unwrap_or_else(|| RType::Tuple(Vec::new()));
                    let fn_ptr_rt = RType::FnPtr {
                        params: entry.param_types.clone(),
                        ret: Box::new(ret),
                    };
                    if let Some(ci) = callee_idx {
                        let id = expr.id as usize;
                        if id < ctx.fn_item_addrs.len() {
                            ctx.fn_item_addrs[id] = Some(ci);
                        }
                    }
                    return Ok(rtype_to_infer(&fn_ptr_rt));
                }
                if template_lookup(ctx.funcs, &path).is_some() {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: format!(
                            "cannot take address of generic fn `{}` as a fn pointer (specify type arguments to monomorphize first)",
                            name
                        ),
                        span: expr.span.copy(),
                    });
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
            // Closure capture-mode upgrade: `&mut Var(captured)` is
            // a mutation observation. Walk the inner to find a root
            // Var; if it's a captured binding, upgrade the capture's
            // mode to RefMut. (Shared `&Var(captured)` doesn't
            // upgrade — read-only borrow leaves the closure FnMut-
            // free.)
            if *mutable {
                if let Some(root_name) = root_var_name_of_place(inner) {
                    let mut idx: Option<usize> = None;
                    let mut i = ctx.locals.len();
                    while i > 0 {
                        i -= 1;
                        if ctx.locals[i].name == root_name {
                            idx = Some(i);
                            break;
                        }
                    }
                    if let Some(idx) = idx {
                        upgrade_capture_to_ref_mut(ctx, root_name, idx);
                    }
                }
            }
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
        ExprKind::Closure(closure) => check_closure(ctx, closure, expr),
    }
}

// `|p1, p2| body` (and `move |...|`) — a closure expression. Type-checks
// the body in a fresh nested scope with each closure param bound to
// either the user's annotated type or a fresh inference variable.
// Outer-binding references are detected via the `closure_scopes` capture
// barrier and rejected in phase 1 with a "closures cannot yet capture"
// error. The expression's type is the synthesized unit struct
// `__closure_<idx>` (registered in StructTable on the fly so other
// typeck pieces — let-binding inference, expr_types finalization — see
// a real struct path). The post-typeck lowering pass reads the side
// table populated here to emit the matching `Item::Struct` and
// `Item::Impl Fn<...>` AST nodes that codegen consumes.
fn check_closure(
    ctx: &mut CheckCtx,
    closure: &Closure,
    closure_expr: &Expr,
) -> Result<InferType, Error> {
    // Allocate a unique struct path. Counter lives on FuncTable so it
    // persists across functions/modules within the same compile() run.
    let idx = ctx.funcs.closure_counter;
    ctx.funcs.closure_counter += 1;
    let mut struct_path: Vec<String> = ctx.current_module.clone();
    struct_path.push(format!("__closure_{}", idx));

    // Bidirectional inference: if the call site stashed an expected
    // signature for this closure (via a `Fn(A) -> R` bound on the
    // matching parameter), use it as the source of param/return
    // types. Annotations still override per-param. Without an
    // expected signature we fall back to an integer-class inference
    // var per unannotated param (the num-lit dispatch covers
    // arithmetic-using bodies; non-numeric closures still need
    // annotations in this fallback path).
    let expected = ctx
        .expected_closure_signatures
        .get(closure_expr.id as usize)
        .and_then(|o| o.clone());
    // Mark the slot consumed so nested closures or repeated visits
    // don't reuse it.
    if (closure_expr.id as usize) < ctx.expected_closure_signatures.len() {
        ctx.expected_closure_signatures[closure_expr.id as usize] = None;
    }
    let mut param_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < closure.params.len() {
        let it = match &closure.params[k].ty {
            Some(t) => {
                let rt = path_resolve::resolve_type(
                    t,
                    ctx.current_module,
                    ctx.structs,
                    ctx.enums,
                    ctx.aliases,
                    ctx.self_target,
                    ctx.type_params,
                    &ctx.use_scope,
                    ctx.reexports,
                    ctx.current_file,
                )?;
                rtype_to_infer(&rt)
            }
            None => match &expected {
                Some((expected_params, _)) if k < expected_params.len() => {
                    expected_params[k].clone()
                }
                _ => InferType::Var(ctx.subst.fresh_int()),
            },
        };
        param_infer.push(it);
        k += 1;
    }

    // Resolve the optional `-> R` return type into an InferType. With
    // no annotation, allocate a fresh var the body's inferred type will
    // be unified against.
    let return_infer: InferType = match &closure.return_type {
        Some(t) => {
            let rt = path_resolve::resolve_type(
                t,
                ctx.current_module,
                ctx.structs,
                ctx.enums,
                ctx.aliases,
                ctx.self_target,
                ctx.type_params,
                &ctx.use_scope,
                ctx.reexports,
                ctx.current_file,
            )?;
            rtype_to_infer(&rt)
        }
        None => match &expected {
            Some((_, expected_return)) => expected_return.clone(),
            None => InferType::Var(ctx.subst.fresh_var()),
        },
    };

    // Push a closure scope frame so Var lookups inside the body can
    // detect captures via the locals barrier.
    let local_barrier = ctx.locals.len();
    ctx.closure_scopes.push(ClosureScope {
        local_barrier,
        node_id: closure_expr.id,
        synthesized_struct_path: struct_path.clone(),
        captures: Vec::new(),
    });

    // Push closure params into locals via pattern check. The common
    // case is a single Binding (`|x|`), but tuple destructures (`|(a,
    // b)|`) and wildcards (`|_|`) are also accepted; refutability is
    // checked against each param's inferred type. Bindings produced by
    // each pattern join the local stack just like let-bindings would.
    let mut k = 0;
    while k < closure.params.len() {
        let param_ty = param_infer[k].clone();
        let pat = &closure.params[k].pattern;
        let mut bindings: Vec<(String, InferType, Span, bool)> = Vec::new();
        check_pattern(ctx, pat, &param_ty, &mut bindings)?;
        if !patterns::pattern_is_irrefutable(ctx, &param_ty, pat) {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "refutable pattern in closure parameter".to_string(),
                span: pat.span.copy(),
            });
        }
        let mut bi = 0;
        while bi < bindings.len() {
            ctx.locals.push(LocalEntry {
                name: bindings[bi].0.clone(),
                ty: bindings[bi].1.clone(),
                mutable: bindings[bi].3,
                declared_uninit: false,
            });
            bi += 1;
        }
        k += 1;
    }

    // Type-check the body. Body's inferred type unifies against the
    // (annotated or fresh-var) return type so call-site context can
    // pin both ends.
    let body_ty = check_expr(ctx, &closure.body)?;
    ctx.subst.coerce(
        &body_ty,
        &return_infer,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &closure.body.span,
        ctx.current_file,
    )?;

    // Pop scope and locals — capture the scope frame's recorded
    // captures before discarding it.
    ctx.locals.truncate(local_barrier);
    let scope = ctx.closure_scopes.pop().expect("just pushed");

    // Record on the side table — finalized into ClosureInfo at end of
    // the enclosing function's typeck.
    let enclosing_type_params: Vec<String> = ctx.type_params.clone();
    ctx.closure_records[closure_expr.id as usize] = Some(PendingClosure {
        synthesized_struct_path: struct_path.clone(),
        param_types: param_infer,
        return_type: return_infer,
        is_move: closure.is_move,
        body_span: closure.body.span.copy(),
        captures: scope.captures,
        enclosing_type_params: enclosing_type_params.clone(),
    });

    // Closure expression's type is the synthesized struct, generic
    // over the enclosing fn's type-params (so `__closure_42<T>`
    // inside `fn helper<T>(...)`). Each enclosing type-param is
    // passed through as `InferType::Param(name)` so substitution at
    // monomorphization time reaches into the synthesized struct's
    // fields and impl methods.
    let type_args: Vec<InferType> = enclosing_type_params
        .iter()
        .map(|n| InferType::Param(n.clone()))
        .collect();
    Ok(InferType::Struct {
        path: struct_path,
        type_args,
        lifetime_args: Vec::new(),
    })
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
    ctx.subst.coerce(
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
    let (span, src_id) = match value {
        Some(e) => (e.span.copy(), e.id),
        None => (expr.span.copy(), expr.id),
    };
    coerce_at(ctx, src_id, &actual, &expected, &span)?;
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
    if let Err(e) = ctx.subst.coerce(
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
    ctx.subst.coerce(
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
    ctx.subst.coerce(
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
            declared_uninit: false,
        });
        k += 1;
    }
    ctx.loop_labels.push(f.label.clone());
    let unit = InferType::Tuple(Vec::new());
    let body_ty = check_block_inner(ctx, f.body.as_ref())?;
    ctx.subst.coerce(
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
    ctx.subst.coerce(
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
    // The if's overall type is the non-`!` arm's type when one arm
    // diverges (so `if cond { panic!() } else { 42 }` types as the
    // else arm's u32, not `!`). `merge` returns the non-Never side
    // when one is Never, otherwise unifies and returns either.
    ctx.subst.merge(
        &then_ty,
        &else_ty,
        ctx.traits,
        &ctx.type_params,
        &ctx.type_param_bounds,
        &outer.span,
        ctx.current_file,
    )
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
                declared_uninit: false,
            });
            k += 1;
        }
        // Guard: a `bool`-typed expression that runs after the pattern
        // matches but before the body. Bindings are in scope.
        if let Some(g) = &arm.guard {
            let g_ty = check_expr(ctx, g)?;
            ctx.subst.coerce(
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
        match arm_ty.take() {
            Some(prev) => {
                let merged = ctx.subst.merge(
                    &body_ty,
                    &prev,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    &arm.body.span,
                    ctx.current_file,
                )?;
                arm_ty = Some(merged);
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
            declared_uninit: false,
        });
        k += 1;
    }
    let then_ty = check_block_expr(ctx, il.then_block.as_ref())?;
    ctx.locals.truncate(mark);
    let else_ty = check_block_expr(ctx, il.else_block.as_ref())?;
    ctx.subst.merge(
        &then_ty,
        &else_ty,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &outer.span,
        ctx.current_file,
    )
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
        ctx.subst.coerce(
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
    ctx.subst.coerce(
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
    ctx.subst.coerce(
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
        ctx.aliases,
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
        ctx.aliases,
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
    ctx.subst.coerce(
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
    ctx.subst.coerce(
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
    ctx.subst.coerce(
        &arg0_ty,
        &expected0,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    let expected1 = rtype_to_infer(&RType::Int(IntKind::Usize));
    ctx.subst.coerce(
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
        ctx.aliases,
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
    ctx.subst.coerce(
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
        ctx.aliases,
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
        ctx.aliases,
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
    ctx.subst.coerce(
        &arg0_ty,
        &expected0,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &args[0].span,
        ctx.current_file,
    )?;
    let expected1 = rtype_to_infer(&RType::Int(IntKind::Usize));
    ctx.subst.coerce(
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
        ctx.aliases,
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
    ctx.subst.coerce(
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
    ctx.subst.coerce(
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
    AliasEntry, AliasTable, CallResolution, CaptureInfo, CaptureMode, ClosureInfo, ConstEntry,
    ConstTable, ConstValue, EnumEntry, EnumTable, EnumVariantEntry, FnSymbol, FuncTable,
    GenericTemplate, LifetimePredResolved, MethodResolution, MoveStatus, MovedPlace, PatternErgo,
    ResolvedVisibility, RTypedField, ReceiverAdjust, StructEntry, StructTable, SupertraitRef,
    TraitDispatch, TraitEntry, TraitImplEntry, TraitMethodEntry, TraitReceiverShape, TraitTable,
    VariantPayloadResolved, alias_lookup, const_lookup, enum_lookup, find_inherent_synth_idx,
    func_lookup, struct_lookup, template_lookup, trait_lookup,
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
    collect_consts, collect_enum_names, collect_funcs, collect_struct_names, collect_trait_names,
    push_root_name, resolve_enum_variants, resolve_impl_target, resolve_struct_fields,
    resolve_trait_methods, resolve_type_aliases, validate_supertrait_obligations,
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
                    let binding_ty = ctx.locals[i].ty.clone();
                    // Place-position Var must record captures the same
                    // way value-position does — see rt3 problem 3.
                    record_capture_if_needed(ctx, name, i);
                    return Ok(binding_ty);
                }
            }
            // Const fallback: same shape as the value-position arm.
            // A const-receiver method call (e.g. binop LHS) lands here
            // first because operator desugar routes through method
            // dispatch which place-checks the receiver. Record the
            // value on `const_uses` so codegen / borrowck inline it,
            // and return the const's type so dispatch proceeds.
            let single = vec![name.clone()];
            let resolved = crate::typeck::use_scope::resolve_via_use_scopes(
                &single,
                &ctx.use_scope,
                |cand| const_lookup(ctx.consts, cand).is_some(),
            )
            .or_else(|| {
                let mut full = ctx.current_module.clone();
                full.push(name.clone());
                if const_lookup(ctx.consts, &full).is_some() {
                    Some(full)
                } else {
                    None
                }
            });
            if let Some(path) = resolved {
                let entry = const_lookup(ctx.consts, &path)
                    .expect("just verified above");
                let ty = rtype_to_infer(&entry.ty);
                let id = expr.id as usize;
                if id < ctx.const_uses.len() {
                    ctx.const_uses[id] = Some(entry.value.clone());
                }
                return Ok(ty);
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
            // the place's address). Use place-mode typing so a ref-typed
            // FieldAccess inner (e.g. `*self.counter` for a closure's
            // RefMut capture field of type `&mut u32`) doesn't trip the
            // value-position move-out-of-borrow check on the field; the
            // field-access-on-ref-of-non-Copy-field rule only applies to
            // value-position reads.
            let inner_ty = check_place_expr(ctx, inner)?;
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
    // Use place-expression typing for the inner: for chains like
    // `Var` / `FieldAccess` / `Deref` / `TupleIndex`, this avoids the
    // value-position "move out of borrow on non-Copy field" check
    // that would otherwise reject `*self.<ref_field>` patterns
    // (which are how RefMut closure captures get derefed in
    // synthesized impl bodies). Non-place inners (e.g. `*foo()`)
    // fall back to value-position check_expr inside check_place_expr.
    let inner_ty = check_place_expr(ctx, inner)?;
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
        ctx.aliases,
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

// Dispatch a bare `f(args)` call where `f` is a local of synthesized
// closure type. Replicates the structure of `check_closure_method_call`
// (in methods.rs) but takes the original `Call` (no `.call` method) +
// resolves to the same `Fn::call` trait dispatch. Records the
// callee's binding name on `ctx.bare_closure_calls[id]` so mono knows
// to lower this Call as a MethodCall MonoExpr.
// `f(args)` where `f: fn(T1, T2) -> R`. Type-check arity + each arg
// against the FnPtr's params, record `PendingCall::Indirect` so the
// finalizer lowers it to `CallResolution::Indirect` for codegen, and
// return the FnPtr's return type.
fn check_indirect_call(
    ctx: &mut CheckCtx,
    call: &Call,
    call_expr: &Expr,
    callee_name: String,
    param_types: Vec<InferType>,
    return_type: InferType,
) -> Result<InferType, Error> {
    if call.args.len() != param_types.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to fn pointer `{}`: expected {}, got {}",
                callee_name,
                param_types.len(),
                call.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let mut k = 0;
    while k < call.args.len() {
        let arg_ty = check_expr(ctx, &call.args[k])?;
        coerce_at(ctx, call.args[k].id, &arg_ty, &param_types[k], &call.args[k].span)?;
        k += 1;
    }
    ctx.call_resolutions[call_expr.id as usize] = Some(PendingCall::Indirect {
        callee_local_name: callee_name,
        param_infers: param_types,
        ret_infer: return_type.clone(),
    });
    Ok(return_type)
}

fn check_bare_closure_call(
    ctx: &mut CheckCtx,
    call: &Call,
    call_expr: &Expr,
    binding_name: String,
    binding_ty: InferType,
) -> Result<InferType, Error> {
    // Look up the closure's recorded signature.
    let target_path = match ctx.subst.substitute(&binding_ty) {
        InferType::Struct { path, .. } => path,
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("internal: bare-call recv `{}` is not a struct", binding_name),
                span: call_expr.span.copy(),
            });
        }
    };
    let mut signature: Option<(Vec<InferType>, InferType)> = None;
    let mut i = 0;
    while i < ctx.closure_records.len() {
        if let Some(pc) = &ctx.closure_records[i] {
            if pc.synthesized_struct_path == target_path {
                signature = Some((pc.param_types.clone(), pc.return_type.clone()));
                break;
            }
        }
        i += 1;
    }
    if signature.is_none() {
        let mut e = 0;
        while e < ctx.funcs.entries.len() {
            let mut k = 0;
            while k < ctx.funcs.entries[e].closures.len() {
                if let Some(ci) = &ctx.funcs.entries[e].closures[k] {
                    if ci.synthesized_struct_path == target_path {
                        let params: Vec<InferType> =
                            ci.param_types.iter().map(rtype_to_infer).collect();
                        let ret = rtype_to_infer(&ci.return_type);
                        signature = Some((params, ret));
                        break;
                    }
                }
                k += 1;
            }
            if signature.is_some() {
                break;
            }
            e += 1;
        }
    }
    let (param_types, return_type) = match signature {
        Some(s) => s,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "internal: no closure record for `{}`",
                    place_to_string(&target_path)
                ),
                span: call_expr.span.copy(),
            });
        }
    };
    if call.args.len() != param_types.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to closure `{}`: expected {}, got {}",
                binding_name,
                param_types.len(),
                call.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    // Type-check each arg, unifying with the closure's stored param
    // types.
    let mut k = 0;
    while k < call.args.len() {
        let arg_ty = check_expr(ctx, &call.args[k])?;
        coerce_at(ctx, call.args[k].id, &arg_ty, &param_types[k], &call.args[k].span)?;
        k += 1;
    }
    // Trait dispatch: Fn::call(&self, (args,)) -> Output.
    let trait_path: Vec<String> = vec![
        "std".to_string(),
        "ops".to_string(),
        "Fn".to_string(),
    ];
    let args_tuple = InferType::Tuple(param_types.clone());
    let recv_struct_infer = InferType::Struct {
        path: target_path,
        type_args: Vec::new(),
        lifetime_args: Vec::new(),
    };
    ctx.method_resolutions[call_expr.id as usize] = Some(PendingMethodCall {
        callee_idx: 0,
        callee_path: Vec::new(),
        recv_adjust: ReceiverAdjust::BorrowImm,
        ret_borrows_receiver: false,
        template_idx: None,
        type_arg_infers: Vec::new(),
        trait_dispatch: Some(PendingTraitDispatch {
            trait_path,
            trait_arg_infers: vec![args_tuple],
            method_name: "call".to_string(),
            recv_type_infer: recv_struct_infer,
            dispatch_span: call_expr.span.copy(),
        }),
    });
    if (call_expr.id as usize) < ctx.bare_closure_calls.len() {
        ctx.bare_closure_calls[call_expr.id as usize] = Some(binding_name);
    }
    let substituted = ctx.subst.substitute(&return_type);
    Ok(infer_concretize_assoc_proj(
        &substituted,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bound_assoc,
    ))
}

// Find the `(params, return)` signature on a type-param's
// `Fn`/`FnMut`/`FnOnce` bound. Walks the (paths, args, assoc)
// triples we keep on `ctx`. Returns None if `param_name` doesn't
// have an Fn-family bound.
// Returns `(matched_trait_path, params, return_ty)` for the matching
// Fn-family bound. The trait path is one of `std::ops::Fn`,
// `std::ops::FnMut`, or `std::ops::FnOnce` — picking the most-
// permissive one the type-param actually carries lets bare-call
// dispatch use the right `call` / `call_mut` / `call_once` method.
// A type-param bounded by only `FnMut` (e.g. via `impl FnMut(u32)`)
// must dispatch via `FnMut::call_mut`, not `Fn::call`, since the
// closure passed in has no `Fn` impl.
fn typeparam_fn_signature(
    ctx: &CheckCtx,
    param_name: &str,
) -> Option<(Vec<String>, Vec<InferType>, InferType)> {
    let mut idx: Option<usize> = None;
    let mut i = 0;
    while i < ctx.type_params.len() {
        if ctx.type_params[i] == param_name {
            idx = Some(i);
            break;
        }
        i += 1;
    }
    let idx = idx?;
    if idx >= ctx.type_param_bounds.len() {
        return None;
    }
    let bounds = &ctx.type_param_bounds[idx];
    let args_rows = ctx.type_param_bound_args.get(idx);
    // Prefer Fn over FnMut over FnOnce when multiple are listed —
    // `Fn` is the strictest and gives the broadest call shape (`&self`).
    // If only FnMut is present, dispatch as FnMut; if only FnOnce,
    // FnOnce.
    let mut best: Option<(usize, &Vec<String>)> = None;
    let mut b = 0;
    while b < bounds.len() {
        let path = &bounds[b];
        let is_fn_family = path.len() == 3
            && path[0] == "std"
            && path[1] == "ops"
            && (path[2] == "Fn" || path[2] == "FnMut" || path[2] == "FnOnce");
        if is_fn_family {
            let priority = match path[2].as_str() {
                "Fn" => 0,
                "FnMut" => 1,
                "FnOnce" => 2,
                _ => 3,
            };
            let pick = match best {
                None => true,
                Some((bp, _)) => priority < bp,
            };
            if pick {
                best = Some((priority, path));
                let _ = b;
            }
        }
        b += 1;
    }
    let (_priority, trait_path_ref) = best?;
    let trait_path = trait_path_ref.clone();
    // Re-find the bound index for trait_args lookup.
    let mut bidx: Option<usize> = None;
    let mut b = 0;
    while b < bounds.len() {
        if bounds[b] == trait_path {
            bidx = Some(b);
            break;
        }
        b += 1;
    }
    let bidx = bidx?;
    let trait_args = args_rows.and_then(|r| r.get(bidx))?;
    if trait_args.is_empty() {
        return None;
    }
    let params: Vec<InferType> = match &trait_args[0] {
        RType::Tuple(elems) => elems.iter().map(rtype_to_infer).collect(),
        _ => return None,
    };
    // `Output` is recorded under the matching bound's slot in the
    // flattened-per-param assoc table. Since `FnOnce` declares
    // `Output` and the others inherit, the assoc entry comes from
    // the trait-arg's matching bound or any of the family's
    // closures (we trust the assoc-table to be correct for the
    // dispatch trait).
    let assoc = ctx.type_param_bound_assoc.get(idx)?;
    let return_ty = assoc
        .iter()
        .find(|(name, _)| name == "Output")
        .map(|(_, rt)| rtype_to_infer(rt))?;
    Some((trait_path, params, return_ty))
}

// Bare-call dispatch for `f(args)` where `f`'s type is a type-param
// with an `Fn`/`FnMut`/`FnOnce` bound (the APIT case). Mirrors
// `check_bare_closure_call`'s structure but the receiver type is
// `InferType::Param(name)` rather than a synthesized closure struct,
// and the bare-call records the binding name on side tables so mono
// lowers as the right method-call shape.
// If `t` is `&dyn Trait` / `&mut dyn Trait` / `Box<dyn Trait>`, return
// the dyn bounds, recv mutability (true if &mut dyn or Box<dyn>), and
// a flag indicating Box receiver. Otherwise None.
fn extract_dyn_fn_bounds(t: &InferType) -> Option<(Vec<InferDynBound>, bool, bool)> {
    if let InferType::Ref { inner, mutable, .. } = t {
        if let InferType::Dyn { bounds, .. } = inner.as_ref() {
            return Some((bounds.clone(), *mutable, false));
        }
    }
    if let InferType::Struct { path, type_args, .. } = t {
        if is_std_box_path(path) && type_args.len() == 1 {
            if let InferType::Dyn { bounds, .. } = &type_args[0] {
                return Some((bounds.clone(), true, true));
            }
        }
    }
    None
}

// `f(args)` where `f: &dyn Fn(...) -> R` / `&mut dyn FnMut(...) -> R`
// / `Box<dyn Fn(...) -> R>`. Routes through DynMethodDispatch on the
// appropriate Fn-family trait method.
fn check_bare_dyn_fn_call(
    ctx: &mut CheckCtx,
    call: &Call,
    call_expr: &Expr,
    binding_name: String,
    bounds: Vec<InferDynBound>,
    recv_mut: bool,
    _recv_is_box: bool,
) -> Result<InferType, Error> {
    if bounds.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "bare-call dispatch on multi-bound `dyn` types not supported".to_string(),
            span: call_expr.span.copy(),
        });
    }
    // Canonicalize and check the bound is a Fn-family trait.
    let trait_path = canonicalize_dyn_trait_path(
        &bounds[0].trait_path,
        ctx.current_module,
        ctx.traits,
        &ctx.use_scope,
        ctx.reexports,
        ctx.current_file,
    );
    let family_name = trait_path.last().cloned().unwrap_or_default();
    let method_name = match family_name.as_str() {
        "Fn" => "call",
        "FnMut" => "call_mut",
        "FnOnce" => "call_once",
        _ => return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot call `{}`: type `&dyn {}` is not callable (no Fn-family trait)",
                binding_name,
                place_to_string(&trait_path),
            ),
            span: call_expr.span.copy(),
        }),
    };
    let entry = match tables::trait_lookup(ctx.traits, &trait_path) {
        Some(e) => e,
        None => return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("trait `{}` not found", place_to_string(&trait_path)),
            span: call_expr.span.copy(),
        }),
    };
    let mut method_idx: Option<usize> = None;
    let mut k = 0;
    while k < entry.methods.len() {
        if entry.methods[k].name == method_name {
            method_idx = Some(k);
            break;
        }
        k += 1;
    }
    let method_idx = method_idx.expect("Fn-family trait should declare its method");
    let method = &entry.methods[method_idx];
    // Pull args from the dyn type's bounds[0].trait_args — Fn family's
    // first trait-arg is the `(T1, T2, ...)` tuple of param types.
    let args_tuple = match bounds[0].trait_args.first() {
        Some(InferType::Tuple(elems)) => elems.clone(),
        _ => Vec::new(),
    };
    if call.args.len() != args_tuple.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected {}, got {}",
                binding_name,
                args_tuple.len(),
                call.args.len(),
            ),
            span: call_expr.span.copy(),
        });
    }
    // Type-check args.
    let mut a = 0;
    while a < call.args.len() {
        let at = check_expr(ctx, &call.args[a])?;
        coerce_at(ctx, call.args[a].id, &at, &args_tuple[a], &call.args[a].span)?;
        a += 1;
    }
    // Resolve the return type: method.return_type is `Self::Output`
    // (an AssocProj). Substitute via the dyn type's assoc bindings.
    let ret_rt = method
        .return_type
        .clone()
        .unwrap_or(RType::Tuple(Vec::new()));
    let assoc_bindings_rt: Vec<(String, RType)> = bounds[0]
        .assoc_bindings
        .iter()
        .map(|(n, t)| (n.clone(), ctx.subst.finalize(t)))
        .collect();
    let ret_rt = substitute_self_assoc(&ret_rt, &assoc_bindings_rt);
    // Method param types (skip receiver at index 0): also substitute.
    // For Fn the second param is `Args: (T1, T2, ...)`; substitute the
    // trait_arg `Args` slot via the dyn type's `trait_args` tuple.
    let trait_args_rt: Vec<RType> = bounds[0]
        .trait_args
        .iter()
        .map(|t| ctx.subst.finalize(t))
        .collect();
    let method_param_types: Vec<RType> = method
        .param_types
        .iter()
        .skip(1)
        .map(|t| {
            let t = substitute_self_assoc(t, &assoc_bindings_rt);
            substitute_trait_args(&t, &entry.trait_type_params, &trait_args_rt)
        })
        .collect();
    let id = call_expr.id as usize;
    if id < ctx.dyn_method_calls.len() {
        ctx.dyn_method_calls[id] = Some(DynMethodDispatch {
            trait_path,
            method_idx: method_idx as u32,
            method_param_types,
            method_return_type: ret_rt.clone(),
            recv_mut,
        });
    }
    // The receiver binding for codegen — same as bare_closure_calls.
    if id < ctx.bare_closure_calls.len() {
        ctx.bare_closure_calls[id] = Some(binding_name);
    }
    Ok(rtype_to_infer(&ret_rt))
}

// Substitute `<Self as ?>::Name` → `binding_value` from
// `assoc_bindings`. Only matches the assoc-projection shape with
// Self base; leaves everything else unchanged.
fn substitute_self_assoc(t: &RType, assoc_bindings: &Vec<(String, RType)>) -> RType {
    match t {
        RType::AssocProj { base, name, .. } => {
            if matches!(base.as_ref(), RType::Param(n) if n == "Self") {
                for (bname, btype) in assoc_bindings {
                    if bname == name {
                        return btype.clone();
                    }
                }
            }
            // No match — recurse into base.
            RType::AssocProj {
                base: Box::new(substitute_self_assoc(base, assoc_bindings)),
                trait_path: match t {
                    RType::AssocProj { trait_path, .. } => trait_path.clone(),
                    _ => Vec::new(),
                },
                name: name.clone(),
            }
        }
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(substitute_self_assoc(inner, assoc_bindings)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(substitute_self_assoc(inner, assoc_bindings)),
            mutable: *mutable,
        },
        RType::Tuple(elems) => RType::Tuple(
            elems.iter().map(|e| substitute_self_assoc(e, assoc_bindings)).collect()
        ),
        RType::Struct { path, type_args, lifetime_args } => RType::Struct {
            path: path.clone(),
            type_args: type_args.iter().map(|a| substitute_self_assoc(a, assoc_bindings)).collect(),
            lifetime_args: lifetime_args.clone(),
        },
        RType::Enum { path, type_args, lifetime_args } => RType::Enum {
            path: path.clone(),
            type_args: type_args.iter().map(|a| substitute_self_assoc(a, assoc_bindings)).collect(),
            lifetime_args: lifetime_args.clone(),
        },
        RType::Slice(inner) => RType::Slice(Box::new(substitute_self_assoc(inner, assoc_bindings))),
        RType::FnPtr { params, ret } => RType::FnPtr {
            params: params.iter().map(|p| substitute_self_assoc(p, assoc_bindings)).collect(),
            ret: Box::new(substitute_self_assoc(ret, assoc_bindings)),
        },
        _ => t.clone(),
    }
}

// Substitute trait-level type-params (`Args` in `Fn<Args>`) using the
// dyn type's trait_args. Maps each `RType::Param(name)` whose name is
// in `trait_type_params` to the matching `trait_args` entry.
fn substitute_trait_args(t: &RType, trait_type_params: &Vec<String>, trait_args: &Vec<RType>) -> RType {
    let env: Vec<(String, RType)> = trait_type_params
        .iter()
        .zip(trait_args.iter())
        .map(|(n, t)| (n.clone(), t.clone()))
        .collect();
    substitute_rtype(t, &env)
}

fn check_bare_typeparam_fn_call(
    ctx: &mut CheckCtx,
    call: &Call,
    call_expr: &Expr,
    binding_name: String,
    param_name: String,
    trait_path: Vec<String>,
    param_types: Vec<InferType>,
    return_type: InferType,
) -> Result<InferType, Error> {
    if call.args.len() != param_types.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected {}, got {}",
                binding_name,
                param_types.len(),
                call.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let mut k = 0;
    while k < call.args.len() {
        let arg_ty = check_expr(ctx, &call.args[k])?;
        coerce_at(ctx, call.args[k].id, &arg_ty, &param_types[k], &call.args[k].span)?;
        k += 1;
    }
    // Pick the matching method name + receiver-adjust per family.
    // Fn → `&self` borrow + `call`, FnMut → `&mut self` + `call_mut`,
    // FnOnce → `self` move + `call_once`. The trait_path arg is the
    // family the type-param actually carries, picked by
    // `typeparam_fn_signature`.
    let (method_name, recv_adjust) = match trait_path.last().map(|s| s.as_str()) {
        Some("FnMut") => ("call_mut".to_string(), ReceiverAdjust::BorrowMut),
        Some("FnOnce") => ("call_once".to_string(), ReceiverAdjust::Move),
        _ => ("call".to_string(), ReceiverAdjust::BorrowImm),
    };
    let args_tuple = InferType::Tuple(param_types.clone());
    let recv_param_infer = InferType::Param(param_name);
    ctx.method_resolutions[call_expr.id as usize] = Some(PendingMethodCall {
        callee_idx: 0,
        callee_path: Vec::new(),
        recv_adjust,
        ret_borrows_receiver: false,
        template_idx: None,
        type_arg_infers: Vec::new(),
        trait_dispatch: Some(PendingTraitDispatch {
            trait_path,
            trait_arg_infers: vec![args_tuple],
            method_name,
            recv_type_infer: recv_param_infer,
            dispatch_span: call_expr.span.copy(),
        }),
    });
    if (call_expr.id as usize) < ctx.bare_closure_calls.len() {
        ctx.bare_closure_calls[call_expr.id as usize] = Some(binding_name);
    }
    let substituted = ctx.subst.substitute(&return_type);
    Ok(infer_concretize_assoc_proj(
        &substituted,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bound_assoc,
    ))
}

fn check_call(ctx: &mut CheckCtx, call: &Call, call_expr: &Expr) -> Result<InferType, Error> {
    // Single-segment callee resolution — locals shadow functions.
    // When the callee is `name(...)` with no path qualification or
    // generic args, AND a local named `name` exists, route by the
    // local's type instead of falling through to the function table:
    //   * synthesized closure struct → bare-call sugar via
    //     `check_bare_closure_call` (records into `bare_closure_calls`
    //     so mono lowers as `local.call((args,))`).
    //   * any other type → `expected function, found <ty>` (matches
    //     rustc E0618). Without this, a `let foo: u32 = …; foo(5)`
    //     would silently call a fn named `foo` if one exists in
    //     scope — see rt3 problem 1.
    // Variant constructors and fn-table lookup only run when no local
    // with that name exists.
    if call.callee.segments.len() == 1
        && call.callee.segments[0].args.is_empty()
        && call.callee.segments[0].lifetime_args.is_empty()
    {
        let name = call.callee.segments[0].name.clone();
        let mut local_ty: Option<InferType> = None;
        let mut i = ctx.locals.len();
        while i > 0 {
            i -= 1;
            if ctx.locals[i].name == name {
                local_ty = Some(ctx.locals[i].ty.clone());
                break;
            }
        }
        if let Some(ty) = local_ty {
            let resolved = ctx.subst.substitute(&ty);
            let is_closure = match &resolved {
                InferType::Struct { path, .. } => path
                    .last()
                    .map(|s| s.starts_with("__closure_"))
                    .unwrap_or(false),
                _ => false,
            };
            if is_closure {
                return check_bare_closure_call(ctx, call, call_expr, name, ty);
            }
            // Local of `fn(...) -> R` type — dispatch as an indirect
            // call. Type-check args against the FnPtr's params, then
            // record `CallResolution::Indirect` so codegen emits args
            // + `local.get callee` + `call_indirect`.
            if let InferType::FnPtr { params, ret } = &resolved {
                return check_indirect_call(ctx, call, call_expr, name, params.clone(), (**ret).clone());
            }
            // Type-param-typed local with an `Fn`/`FnMut`/`FnOnce`
            // bound — the canonical APIT path: `fn apply(f: impl
            // Fn(usize) -> Foo) { f(idx) }`. The desugar in the parser
            // turned `f`'s type into a fresh type-param with the Fn
            // bound, so we can read the signature off the bound and
            // dispatch via the matching family trait.
            if let InferType::Param(param_name) = &resolved {
                if let Some((trait_path, param_types, return_type)) =
                    typeparam_fn_signature(ctx, param_name)
                {
                    return check_bare_typeparam_fn_call(
                        ctx,
                        call,
                        call_expr,
                        name,
                        param_name.clone(),
                        trait_path,
                        param_types,
                        return_type,
                    );
                }
            }
            // Local of `&dyn Fn(...)` / `&mut dyn FnMut(...)` /
            // `Box<dyn Fn(...)>` type — dispatch as a dyn-method call
            // on `call`/`call_mut`/`call_once`. The dyn type's bound
            // list carries the `Args` tuple (positional trait_args)
            // and `Output` (assoc binding) needed to substitute the
            // method signature.
            if let Some((bounds, recv_mut, recv_is_box)) = extract_dyn_fn_bounds(&resolved) {
                return check_bare_dyn_fn_call(
                    ctx, call, call_expr, name, bounds, recv_mut, recv_is_box,
                );
            }
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "expected function, found local `{}` of type `{}`",
                    name,
                    infer_to_string(&resolved),
                ),
                span: call_expr.span.copy(),
            });
        }
    }
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
        if !is_visible_from(&entry.vis, ctx.current_module, ctx.current_crate) {
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
            coerce_at(ctx, call.args[i].id, &arg_ty, &param_infer[i], &call.args[i].span)?;
            i += 1;
        }
        return Ok(return_infer);
    }
    // Try a generic template.
    if let Some((template_idx, _)) = template_lookup(ctx.funcs, &full) {
        let tmpl_vis = ctx.funcs.templates[template_idx].vis.clone();
        let tmpl_path = ctx.funcs.templates[template_idx].path.clone();
        if !is_visible_from(&tmpl_vis, ctx.current_module, ctx.current_crate) {
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
        // Snapshot bounds + bound-trait-args for bidirectional
        // inference into closure args.
        let tmpl_bound_paths_for_inference: Vec<Vec<Vec<String>>> =
            ctx.funcs.templates[template_idx].type_param_bounds.clone();
        let tmpl_bound_args_for_inference: Vec<Vec<Vec<RType>>> =
            ctx.funcs.templates[template_idx].type_param_bound_args.clone();
        let tmpl_bound_assoc_for_inference: Vec<Vec<Vec<(String, RType)>>> =
            ctx.funcs.templates[template_idx].type_param_bound_assoc.clone();
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
        // Apply explicit turbofish args by unifying. Each arg may
        // contain `_` placeholders; resolve via the inference-aware
        // path so they become fresh InferVars rather than errors.
        let mut k = 0;
        while k < last_seg_args.len() {
            let user_infer = resolve_type_to_infer(ctx, &last_seg_args[k])?;
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
            // Bidirectional inference into closure args: if this arg
            // is a closure expression and the corresponding param's
            // template type is `Param("F")` whose bound is one of
            // `Fn`/`FnMut`/`FnOnce` with a concrete `(P,) -> R`
            // signature, stash the (params, return) on the side table
            // so `check_closure` adopts those types instead of fresh
            // vars.
            if matches!(call.args[i].kind, ExprKind::Closure(_)) {
                if let RType::Param(param_name) = &tmpl_param_types[i] {
                    if let Some((expected_params, expected_return)) = lookup_fn_bound_signature(
                        param_name,
                        &tmpl_type_params,
                        &tmpl_bound_paths_for_inference,
                        &tmpl_bound_args_for_inference,
                        &tmpl_bound_assoc_for_inference,
                    ) {
                        let id = call.args[i].id as usize;
                        if id < ctx.expected_closure_signatures.len() {
                            ctx.expected_closure_signatures[id] =
                                Some((expected_params, expected_return));
                        }
                    }
                }
            }
            let arg_ty = check_expr(ctx, &call.args[i])?;
            coerce_at(ctx, call.args[i].id, &arg_ty, &param_infer[i], &call.args[i].span)?;
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
            // Skip the assoc-constraint check for synthesized closure
            // struct types — their `Fn`/`FnMut`/`FnOnce` impl is
            // registered post-typeck by `closure_lower::lower`, so
            // the impl's `Output` binding isn't yet visible. The
            // body-check enforces the closure's actual return type
            // matches what bidirectional inference flowed in, so the
            // bound is satisfied by construction once the impl
            // lands.
            let recv_is_closure = matches!(
                &inferred_rt,
                RType::Struct { path, .. } if path
                    .last()
                    .map(|s| s.starts_with("__closure_"))
                    .unwrap_or(false)
            );
            if recv_is_closure {
                p += 1;
                continue;
            }
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
        // Where-clause predicates with complex LHS. After substitution
        // the LHS is a concrete RType; each bound must have a
        // resolvable impl.
        let tmpl_where = ctx.funcs.templates[template_idx].where_predicates.clone();
        let mut wp = 0;
        while wp < tmpl_where.len() {
            let pred = &tmpl_where[wp];
            let lhs_substituted = substitute_rtype(&pred.lhs, &subst_env);
            let mut bk = 0;
            while bk < pred.bounds.len() {
                let b = &pred.bounds[bk];
                let trait_args_subst: Vec<RType> = b
                    .trait_args
                    .iter()
                    .map(|t| substitute_rtype(t, &subst_env))
                    .collect();
                if traits::solve_impl_in_ctx_with_args(
                    &b.trait_path,
                    &trait_args_subst,
                    &lhs_substituted,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bounds,
                    0,
                )
                .is_none()
                {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: format!(
                            "where-clause predicate not satisfied at call site: `{}: {}` has no matching impl",
                            rtype_to_string(&lhs_substituted),
                            place_to_string(&b.trait_path),
                        ),
                        span: call_expr.span.copy(),
                    });
                }
                bk += 1;
            }
            wp += 1;
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
    if !is_visible_from(&entry.vis, ctx.current_module, ctx.current_crate) {
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
                    vis: fields[k].vis.clone(),
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
                ctx.aliases,
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
        ctx.subst.coerce(
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
    if !is_visible_from(&entry.vis, ctx.current_module, ctx.current_crate) {
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
            let infer = resolve_type_to_infer(ctx, &explicit_type_args[k])?;
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
        ctx.subst.coerce(
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
    if !is_visible_from(&entry.vis, ctx.current_module, ctx.current_crate) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("struct `{}` is private", place_to_string(&entry.path)),
            span: lit.path.span.copy(),
        });
    }
    let struct_type_params: Vec<String> = entry.type_params.clone();
    let mut def_field_names: Vec<String> = Vec::new();
    let mut def_field_types: Vec<RType> = Vec::new();
    let mut def_field_vis: Vec<crate::typeck::ResolvedVisibility> = Vec::new();
    let mut k = 0;
    while k < entry.fields.len() {
        def_field_names.push(entry.fields[k].name.clone());
        def_field_types.push(entry.fields[k].ty.clone());
        def_field_vis.push(entry.fields[k].vis.clone());
        k += 1;
    }
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
        let user_infer = resolve_type_to_infer(ctx, &last_seg.args[k])?;
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
        if !field_visible_from(&def_field_vis[found_idx], ctx.current_module, ctx.current_crate) {
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
                ctx.subst.coerce(
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
            if !field_visible_from(&entry.fields[i].vis, ctx.current_module, ctx.current_crate) {
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
