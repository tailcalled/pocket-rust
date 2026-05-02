use super::{
    EnumEntry, EnumTable, EnumVariantEntry, FnSymbol, FuncTable, GenericTemplate, LifetimeRepr,
    RType, RTypedField, ReExportTable, StructEntry, StructTable, TraitEntry, TraitImplEntry,
    TraitMethodEntry, TraitReceiverShape, TraitTable, UseEntry, VariantPayloadResolved, copy_trait_path, drop_trait_path,
    find_elision_source, freshen_inferred_lifetimes, func_lookup,
    is_copy_with_bounds, is_visible_from, module_use_entries, outer_lifetime, place_to_string,
    require_no_inferred_lifetimes, resolve_type, resolve_via_use_scopes,
    rtype_eq, rtype_to_string, segments_to_string, solve_impl_in_ctx, struct_env, struct_lookup, substitute_rtype, template_lookup, trait_lookup,
    trait_lookup_resolved, type_defining_module, validate_named_lifetimes,
};
use crate::ast::{Function, Item, Module};
use crate::span::{Error, Span};

pub(super) fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

// First-pass trait collection. Records each `trait Foo { fn ... ; }` with
// shell `TraitMethodEntry` placeholders (names + spans only). Full
// signature resolution happens in `resolve_trait_methods` after structs
// are resolved.
pub(super) fn collect_trait_names(module: &Module, path: &mut Vec<String>, table: &mut TraitTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Trait(td) => {
                let mut full = path.clone();
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
                let mut assoc_type_names: Vec<String> = Vec::new();
                let mut at_i = 0;
                while at_i < td.assoc_types.len() {
                    assoc_type_names.push(td.assoc_types[at_i].name.clone());
                    at_i += 1;
                }
                table.entries.push(TraitEntry {
                    path: full,
                    name_span: td.name_span.copy(),
                    file: module.source_file.clone(),
                    methods,
                    is_pub: td.is_pub,
                    supertraits: Vec::new(),
                    assoc_types: assoc_type_names,
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
pub(super) fn resolve_trait_methods(
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
                let mut full = path.clone();
                full.push(td.name.clone());
                // `Self` placeholder visible inside trait method sigs.
                let self_target = RType::Param("Self".to_string());
                // Find this trait's table entry index so we can mutate
                // its method list after resolving.
                let mut entry_idx: Option<usize> = None;
                let mut e = 0;
                while e < traits.entries.len() {
                    if traits.entries[e].path == full {
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

pub(super) fn collect_struct_names(module: &Module, path: &mut Vec<String>, table: &mut StructTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = path.clone();
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
pub(super) fn collect_enum_names(module: &Module, path: &mut Vec<String>, table: &mut EnumTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Enum(ed) => {
                let mut full = path.clone();
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
pub(super) fn resolve_enum_variants(
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
                let mut full = path.clone();
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
                        if table.entries[e].path == full {
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

pub(super) fn resolve_struct_fields(
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
                let mut full = path.clone();
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
                    if table.entries[e].path == full {
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

pub(super) fn collect_funcs(
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
                        // Resolve & validate `type Name = T;` bindings
                        // against the trait's declared assoc_types. Must
                        // cover every name, no extras, no duplicates.
                        let assoc_bindings = resolve_and_validate_assoc_bindings(
                            ib,
                            &trait_full,
                            &target_rt,
                            path,
                            structs,
                            enums,
                            traits,
                            &impl_type_params,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        let idx = traits.impls.len();
                        register_trait_impl(
                            ib,
                            &trait_full,
                            target_rt.clone(),
                            &impl_type_params,
                            &impl_lifetime_params,
                            &impl_type_param_bounds,
                            assoc_bindings,
                            traits,
                            &module.source_file,
                        )?;
                        // T2.5: `impl Copy for SomeStruct {}` (concrete or
                        // generic) requires every field's type to be Copy.
                        // Generic impls use the impl-type-param bounds, so
                        // `impl<T: Copy> Copy for Wrap<T> {}` works.
                        if trait_full == copy_trait_path() {
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
                        // Inherent impls can't declare associated types.
                        if !ib.assoc_type_bindings.is_empty() {
                            return Err(Error {
                                file: module.source_file.clone(),
                                message: "associated type bindings are only allowed in trait impls"
                                    .to_string(),
                                span: ib.assoc_type_bindings[0].name_span.copy(),
                            });
                        }
                        None
                    };
                // Method-path prefix. Mirror codegen's derivation: take the
                // first segment of the target's AST Path. For non-Path
                // trait impls (`impl<T> Show for &T`, …) synthesize a slot
                // from the trait-impl row. For non-Path inherent impls
                // (`impl<T> *const T { … }`) synthesize a slot from
                // `funcs.inherent_synth_count`.
                let target_name_for_prefix: Option<String> = match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if !p.segments.is_empty() => {
                        Some(p.segments[0].name.clone())
                    }
                    _ => None,
                };
                let mut method_prefix = path.clone();
                if let Some(name) = &target_name_for_prefix {
                    method_prefix.push(name.clone());
                } else if let Some(idx) = trait_impl_idx_for_methods {
                    method_prefix.push(format!("__trait_impl_{}", idx));
                } else {
                    let idx = funcs.inherent_synth_specs.len();
                    funcs.inherent_synth_specs
                        .push((module.source_file.clone(), ib.span.copy()));
                    method_prefix.push(format!("__inherent_synth_{}", idx));
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
pub(super) fn find_trait_impl_idx(
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
        if traits.impls[i].trait_path == trait_full
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
pub(super) fn resolve_trait_path(
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
        let mut full = current_module.clone();
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
            return Ok(entry.path.clone());
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
        let mut full = method_prefix.clone();
        full.push(m_name.clone());
        let (impl_param_types, impl_return_type) =
            if let Some(entry) = func_lookup(funcs, &full) {
                (
                    entry.param_types.clone(),
                    entry.return_type.clone(),
                )
            } else if let Some((_, t)) = template_lookup(funcs, &full) {
                (
                    t.param_types.clone(),
                    t.return_type.clone(),
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
            vec![("Self".to_string(), target_rt.clone())];
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
            let subst = substitute_rtype(&trait_method.param_types[p], &trait_env);
            // After Self gets substituted to the impl target, any
            // `Self::Item` projection now points at a concrete type —
            // resolve through the impl's bindings so the comparison
            // against the impl method's signature lines up.
            expected_param_types.push(crate::typeck::concretize_assoc_proj(&subst, traits));
            p += 1;
        }
        let expected_return_type: Option<RType> = trait_method
            .return_type
            .as_ref()
            .map(|rt| {
                let subst = substitute_rtype(rt, &trait_env);
                crate::typeck::concretize_assoc_proj(&subst, traits)
            });
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
    assoc_type_bindings: Vec<(String, RType)>,
    traits: &mut TraitTable,
    file: &str,
) -> Result<(), Error> {
    let mut i = 0;
    while i < traits.impls.len() {
        if &traits.impls[i].trait_path == trait_full
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
    let conflicting_path: Option<Vec<String>> = if trait_full == &copy_path {
        Some(drop_path.clone())
    } else if trait_full == &drop_path {
        Some(copy_path.clone())
    } else {
        None
    };
    if let Some(other) = conflicting_path {
        let mut i = 0;
        while i < traits.impls.len() {
            if traits.impls[i].trait_path == other
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
        trait_path: trait_full.clone(),
        target,
        impl_type_params: impl_type_params.clone(),
        impl_lifetime_params: impl_lifetime_params.clone(),
        impl_type_param_bounds: bounds_clone,
        assoc_type_bindings,
        file: file.to_string(),
        span: ib.span.copy(),
    });
    Ok(())
}

// Resolves each `type Name = T;` binding and verifies that the impl
// covers exactly the trait's `assoc_types` — no missing, no extras, no
// duplicates. Returns the resolved bindings in the trait's declared
// order. Inherent impls (no `trait_full`) aren't allowed to declare
// assoc bindings; the caller only routes here for trait impls.
pub(super) fn resolve_and_validate_assoc_bindings(
    ib: &crate::ast::ImplBlock,
    trait_full: &Vec<String>,
    target_rt: &RType,
    current_module: &Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    impl_type_params: &Vec<String>,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Result<Vec<(String, RType)>, Error> {
    // Look up the trait to know its declared assoc_types.
    let trait_entry = match trait_lookup(traits, trait_full) {
        Some(e) => e,
        None => unreachable!("validate_trait_impl already ensured trait exists"),
    };
    // Reject duplicates first — a Name listed twice in the impl body
    // is always an error regardless of trait declaration.
    let mut i = 0;
    while i < ib.assoc_type_bindings.len() {
        let mut j = i + 1;
        while j < ib.assoc_type_bindings.len() {
            if ib.assoc_type_bindings[i].name == ib.assoc_type_bindings[j].name {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "duplicate associated type binding `{}` in impl",
                        ib.assoc_type_bindings[j].name
                    ),
                    span: ib.assoc_type_bindings[j].name_span.copy(),
                });
            }
            j += 1;
        }
        i += 1;
    }
    // Reject extras (binding for a name the trait doesn't declare).
    let mut bi = 0;
    while bi < ib.assoc_type_bindings.len() {
        let bname = &ib.assoc_type_bindings[bi].name;
        if !trait_entry.assoc_types.contains(bname) {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "associated type `{}` is not a member of trait `{}`",
                    bname,
                    place_to_string(trait_full)
                ),
                span: ib.assoc_type_bindings[bi].name_span.copy(),
            });
        }
        bi += 1;
    }
    // Reject missing — every declared assoc_type must have a binding.
    let mut ti = 0;
    while ti < trait_entry.assoc_types.len() {
        let want = &trait_entry.assoc_types[ti];
        let mut found = false;
        let mut bi = 0;
        while bi < ib.assoc_type_bindings.len() {
            if &ib.assoc_type_bindings[bi].name == want {
                found = true;
                break;
            }
            bi += 1;
        }
        if !found {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "missing associated type binding `{}` in impl of `{}` for `{}`",
                    want,
                    place_to_string(trait_full),
                    rtype_to_string(target_rt)
                ),
                span: ib.span.copy(),
            });
        }
        ti += 1;
    }
    // Resolve each binding's RHS type, in trait-declared order.
    let mut out: Vec<(String, RType)> = Vec::new();
    let mut ti = 0;
    while ti < trait_entry.assoc_types.len() {
        let want = trait_entry.assoc_types[ti].clone();
        let mut bi = 0;
        while bi < ib.assoc_type_bindings.len() {
            if ib.assoc_type_bindings[bi].name == want {
                let rt = resolve_type(
                    &ib.assoc_type_bindings[bi].ty,
                    current_module,
                    structs,
                    enums,
                    Some(target_rt),
                    impl_type_params,
                    use_scope,
                    reexports,
                    file,
                )?;
                out.push((want.clone(), rt));
                break;
            }
            bi += 1;
        }
        ti += 1;
    }
    Ok(out)
}

// Walks every registered `impl Trait for T` and verifies that for each
// supertrait `S` of `Trait`, there is also an `impl S for T`. Done after
// all impls are registered (in any source order). The impl-target may be
// a generic pattern with `Param(name)` slots; supertrait checks consult
// the impl's own type-param bounds via `solve_impl_in_ctx` so that
// `impl<T: PartialEq> Eq for Wrap<T>` is satisfied by the generic
// `impl<T: PartialEq> PartialEq for Wrap<T>` row.
pub(super) fn validate_supertrait_obligations(traits: &TraitTable) -> Result<(), Error> {
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
pub(super) fn resolve_impl_target(
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
        // Inherent: must be a struct, enum, raw pointer, or slice. The
        // primitive-pointer methods in `lib/std/primitive/pointer.rs`
        // are inherent on `*const T` / `*mut T`; slice methods (`len`,
        // `get`, …) are inherent on `[T]`. Refs, primitives, and
        // tuples can't carry inherent methods — those go through trait
        // impls.
        match &resolved {
            RType::Struct { .. }
            | RType::Enum { .. }
            | RType::RawPtr { .. }
            | RType::Slice(_)
            | RType::Str => {}
            _ => {
                return Err(Error {
                    file: file.to_string(),
                    message: "inherent impl target must be a struct, enum, raw pointer, slice, or str"
                        .to_string(),
                    span: ib.target.span.copy(),
                });
            }
        }
    }
    Ok(resolved)
}

pub(super) fn register_function(
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
    let mut full = path_prefix.clone();
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
        // Concretize Self::Item / T::Item projections via the trait
        // table (which by this point has the impl's bindings).
        let rt = crate::typeck::concretize_assoc_proj(&rt, traits);
        param_types.push(rt);
        k += 1;
    }
    let mut return_type = match &f.return_type {
        Some(ty) => Some({
            let rt = resolve_type(
                ty,
                current_module,
                structs,
                enums,
                self_target,
                &type_param_names,
                use_scope,
                reexports,
                source_file,
            )?;
            crate::typeck::concretize_assoc_proj(&rt, traits)
        }),
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
    let impl_target_for_storage: Option<RType> = self_target.cloned();
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
    // Per-type-param `Trait<Name = T, ...>` constraints. Aligned to
    // `type_param_bounds` (impl-level slots first — currently always
    // empty since impl bounds don't carry assoc constraints yet —
    // followed by fn-level slots resolved from `f.type_params`).
    let mut type_param_bound_assoc: Vec<Vec<Vec<(String, RType)>>> = Vec::new();
    let mut i = 0;
    while i < impl_type_param_bounds.len() {
        let mut row: Vec<Vec<(String, RType)>> = Vec::new();
        let mut j = 0;
        while j < impl_type_param_bounds[i].len() {
            row.push(Vec::new());
            j += 1;
        }
        type_param_bound_assoc.push(row);
        i += 1;
    }
    let mut i = 0;
    while i < f.type_params.len() {
        let mut row: Vec<Vec<(String, RType)>> = Vec::new();
        let mut j = 0;
        while j < f.type_params[i].bounds.len() {
            let mut constraints: Vec<(String, RType)> = Vec::new();
            let mut c = 0;
            while c < f.type_params[i].bounds[j].assoc_constraints.len() {
                let ac = &f.type_params[i].bounds[j].assoc_constraints[c];
                let cty = resolve_type(
                    &ac.ty,
                    current_module,
                    structs,
                    enums,
                    self_target,
                    &type_param_names,
                    use_scope,
                    reexports,
                    source_file,
                )?;
                constraints.push((ac.name.clone(), cty));
                c += 1;
            }
            row.push(constraints);
            j += 1;
        }
        type_param_bound_assoc.push(row);
        i += 1;
    }
    if is_generic {
        funcs.templates.push(GenericTemplate {
            path: full,
            type_params: type_param_names,
            type_param_bounds,
            type_param_bound_assoc,
            impl_type_param_count: impl_type_params.len(),
            func: f.clone(),
            enclosing_module: current_module.clone(),
            source_file: source_file.to_string(),
            param_types,
            return_type,
            expr_types: Vec::new(),
            param_lifetimes,
            ret_lifetime,
            impl_target: impl_target_for_storage,
            trait_impl_idx,
            is_pub: f.is_pub,
            is_unsafe: f.is_unsafe,
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
            moved_places: Vec::new(),
            move_sites: Vec::new(),
            builtin_type_targets: Vec::new(),
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
            is_unsafe: f.is_unsafe,
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
            moved_places: Vec::new(),
            move_sites: Vec::new(),
            builtin_type_targets: Vec::new(),
        });
        *next_idx += 1;
    }
    Ok(())
}
