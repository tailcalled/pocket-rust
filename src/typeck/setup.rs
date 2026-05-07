use super::{
    AliasEntry, AliasTable, EnumEntry, EnumTable, EnumVariantEntry, FnSymbol, FuncTable,
    GenericTemplate, LifetimeRepr, RType, RTypedField, ReExportTable, StructEntry, StructTable,
    SupertraitRef, TraitEntry, TraitImplEntry, TraitMethodEntry, TraitReceiverShape, TraitTable,
    UseEntry, VariantPayloadResolved, copy_trait_path, drop_trait_path, find_elision_source,
    freshen_inferred_lifetimes, func_lookup, is_copy_with_bounds, is_visible_from,
    module_use_entries, outer_lifetime, place_to_string, require_no_inferred_lifetimes,
    resolve_type, resolve_via_use_scopes, rtype_eq, rtype_to_string, segments_to_string,
    struct_env, struct_lookup, substitute_rtype, supertrait_closure, template_lookup,
    trait_lookup, trait_lookup_resolved, type_defining_module, validate_named_lifetimes,
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
                        type_param_bounds: Vec::new(),
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
                let trait_type_params: Vec<String> =
                    td.type_params.iter().map(|p| p.name.clone()).collect();
                // Defaults are resolved later in resolve_trait_methods
                // (where the `Self` placeholder is in scope and other
                // user types are visible). At collection time we just
                // remember which slots have a default written.
                let mut trait_type_param_defaults: Vec<Option<RType>> =
                    Vec::with_capacity(td.type_params.len());
                let mut tp = 0;
                while tp < td.type_params.len() {
                    trait_type_param_defaults.push(None);
                    tp += 1;
                }
                table.entries.push(TraitEntry {
                    path: full,
                    name_span: td.name_span.copy(),
                    file: module.source_file.clone(),
                    methods,
                    is_pub: td.is_pub,
                    supertraits: Vec::new(),
                    assoc_types: assoc_type_names,
                    trait_type_params,
                    trait_type_param_defaults,
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
            Item::TypeAlias(_) => {}
        }
        i += 1;
    }
}

// Walk the module tree and register every `pub? type Name<...>? = T;`
// alias. Each alias's target gets resolved against the prior aliases
// (declaration-order only — backward references work, forward ones
// fail with the standard "unknown type" diagnostic). Runs before
// struct/enum field resolution so subsequent type lookups see aliases
// in the path table.
pub(super) fn resolve_type_aliases(
    module: &Module,
    path: &mut Vec<String>,
    root_crate_name: &str,
    aliases: &mut AliasTable,
    structs: &StructTable,
    enums: &EnumTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = root_crate_name;
    let use_scope = module_use_entries(module, crate_root);
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::TypeAlias(ta) => {
                let mut full = path.clone();
                full.push(ta.name.clone());
                let type_param_names: Vec<String> =
                    ta.type_params.iter().map(|p| p.name.clone()).collect();
                let lifetime_param_names: Vec<String> =
                    ta.lifetime_params.iter().map(|p| p.name.clone()).collect();
                let target = resolve_type(
                    &ta.target,
                    path,
                    structs,
                    enums,
                    aliases,
                    None,
                    &type_param_names,
                    &use_scope,
                    reexports,
                    &module.source_file,
                )?;
                aliases.entries.push(AliasEntry {
                    path: full,
                    name_span: ta.name_span.copy(),
                    file: module.source_file.clone(),
                    type_params: type_param_names,
                    lifetime_params: lifetime_param_names,
                    target,
                    is_pub: ta.is_pub,
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_type_aliases(m, path, root_crate_name, aliases, structs, enums, reexports)?;
                path.pop();
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

// Second pass over trait declarations: resolve each method's full
// signature using `Self` as `RType::Param("Self")`, classify the
// receiver shape, and store back into `TraitTable.entries`. Runs after
// structs are resolved so method param/return types can reference user
// types.
pub(super) fn resolve_trait_methods(
    module: &Module,
    path: &mut Vec<String>,
    root_crate_name: &str,
    traits: &mut TraitTable,
    structs: &StructTable,
    enums: &EnumTable,
    aliases: &AliasTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = root_crate_name;
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
                // Supertrait edges: resolve each `Trait<X, Y, …>` to a
                // canonical path + concrete arg types. Args reference
                // the trait's own type-params (and `Self`); the
                // obligation check substitutes them per impl row.
                // Assoc-type constraints on supertrait bounds (e.g.
                // `trait Foo: Bar<Item = u32>`) are still a follow-up.
                let trait_type_params_names: Vec<String> = td
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let self_target_for_sup = RType::Param("Self".to_string());
                let mut supertraits: Vec<SupertraitRef> = Vec::new();
                let mut s = 0;
                while s < td.supertraits.len() {
                    let (resolved_path, args) = resolve_trait_ref(
                        &td.supertraits[s].path,
                        path,
                        structs,
                        enums,
                        aliases,
                        Some(&self_target_for_sup),
                        &trait_type_params_names,
                        traits,
                        &use_scope,
                        reexports,
                        &module.source_file,
                    )?;
                    supertraits.push(SupertraitRef { path: resolved_path, args });
                    s += 1;
                }
                traits.entries[entry_idx].supertraits = supertraits;
                // Resolve trait-level type-param defaults (`Rhs = Self`).
                // `Self` is in scope (= self_target = `Param("Self")`),
                // and earlier trait-params are also visible so a later
                // default can reference an earlier one.
                let mut tp = 0;
                while tp < td.type_params.len() {
                    if let Some(default_ty) = &td.type_params[tp].default {
                        let rt = resolve_type(
                            default_ty,
                            path,
                            structs,
                            enums,
                            aliases,
                            Some(&self_target),
                            &trait_type_params_names,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        traits.entries[entry_idx].trait_type_param_defaults[tp] = Some(rt);
                    }
                    tp += 1;
                }
                let mut k = 0;
                while k < td.methods.len() {
                    let m = &td.methods[k];
                    // In-scope type-params for resolving the method
                    // signature: trait's `<Rhs, ...>` first, then
                    // method's own `<U, ...>`. `Self` is the implicit
                    // self_target.
                    let mut type_params: Vec<String> = td
                        .type_params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect();
                    let mut tp_i = 0;
                    while tp_i < m.type_params.len() {
                        type_params.push(m.type_params[tp_i].name.clone());
                        tp_i += 1;
                    }
                    let mut param_types: Vec<RType> = Vec::new();
                    let mut p = 0;
                    while p < m.params.len() {
                        let rt = resolve_type(
                            &m.params[p].ty,
                            path,
                            structs,
                            enums,
                            aliases,
                            Some(&self_target),
                            &type_params,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        param_types.push(rt);
                        p += 1;
                    }
                    // RPIT-aware return-type resolution. Each `impl
                    // Trait` slot in the trait method sig becomes an
                    // `RType::Opaque{<trait>::<method>, slot}` (no
                    // pin — pins live on each impl method's own
                    // FnSymbol). Bounds resolved here for future
                    // validation.
                    let mut rpit_synth_names: Vec<String> = Vec::new();
                    let mut rpit_slot_bounds_ast: Vec<Vec<crate::ast::TraitBound>> =
                        Vec::new();
                    let return_type = match &m.return_type {
                        Some(ty) => {
                            let rewritten = rewrite_rpit_in_type(
                                ty,
                                &mut rpit_synth_names,
                                &mut rpit_slot_bounds_ast,
                            );
                            let mut extended_params = type_params.clone();
                            let mut s = 0;
                            while s < rpit_synth_names.len() {
                                extended_params.push(rpit_synth_names[s].clone());
                                s += 1;
                            }
                            let raw = resolve_type(
                                &rewritten,
                                path,
                                structs,
                                enums,
                                aliases,
                                Some(&self_target),
                                &extended_params,
                                &use_scope,
                                reexports,
                                &module.source_file,
                            )?;
                            // Unique fn_path for the trait method's
                            // opaques: trait full path + method name.
                            let mut method_fn_path = full.clone();
                            method_fn_path.push(m.name.clone());
                            Some(substitute_rpit_synths_to_opaque(
                                &raw,
                                &rpit_synth_names,
                                &method_fn_path,
                            ))
                        }
                        None => None,
                    };
                    let receiver_shape = if !m.params.is_empty() && m.params[0].name == "self" {
                        Some(classify_receiver_shape(&param_types[0]))
                    } else {
                        None
                    };
                    // `Self::Output` (and similar) parsed with empty
                    // trait_path — fill in the supertrait-closure
                    // member that actually declares the assoc, so
                    // downstream `find_assoc_binding` disambiguates
                    // when multiple traits share the same assoc name
                    // (e.g. Add/Sub/Mul/... all declare `Output` for
                    // u8). For inherited assocs (`IndexMut` method
                    // returning `Self::Output` where Output lives on
                    // Index), the closure walk picks `Index`.
                    let mut filled_params: Vec<RType> = Vec::new();
                    let mut p2 = 0;
                    while p2 < param_types.len() {
                        filled_params.push(fill_assoc_trait_path_via_closure(&param_types[p2], &full, traits));
                        p2 += 1;
                    }
                    let filled_return = return_type
                        .as_ref()
                        .map(|rt| fill_assoc_trait_path_via_closure(rt, &full, traits));
                    // Resolve method type-param bounds. The slots
                    // for the trait's own type-params come first
                    // (always empty here — trait-level bounds aren't
                    // tracked here), followed by the method's own
                    // `<U: Bound>` bounds, plus any merge from the
                    // method's where-clause Param-LHS predicates.
                    let mut method_bounds: Vec<Vec<Vec<String>>> =
                        td.type_params.iter().map(|_| Vec::new()).collect();
                    let mut tp = 0;
                    while tp < m.type_params.len() {
                        let mut row: Vec<Vec<String>> = Vec::new();
                        let mut bj = 0;
                        while bj < m.type_params[tp].bounds.len() {
                            let resolved = resolve_trait_path(
                                &m.type_params[tp].bounds[bj].path,
                                path,
                                traits,
                                &use_scope,
                                reexports,
                                &module.source_file,
                            )?;
                            row.push(resolved);
                            bj += 1;
                        }
                        method_bounds.push(row);
                        tp += 1;
                    }
                    // Merge where-clause Param-LHS preds.
                    let mut wi = 0;
                    while wi < m.where_clause.len() {
                        if let crate::ast::WherePredicate::Type {
                            lhs, bounds: wbounds, ..
                        } = &m.where_clause[wi]
                        {
                            let lhs_rt = resolve_type(
                                lhs,
                                path,
                                structs,
                                enums,
                                aliases,
                                Some(&self_target),
                                &type_params,
                                &use_scope,
                                reexports,
                                &module.source_file,
                            )?;
                            if let RType::Param(name) = &lhs_rt {
                                let mut idx: Option<usize> = None;
                                let mut k2 = 0;
                                while k2 < type_params.len() {
                                    if &type_params[k2] == name {
                                        idx = Some(k2);
                                        break;
                                    }
                                    k2 += 1;
                                }
                                if let Some(idx) = idx {
                                    let mut bj = 0;
                                    while bj < wbounds.len() {
                                        let resolved = resolve_trait_path(
                                            &wbounds[bj].path,
                                            path,
                                            traits,
                                            &use_scope,
                                            reexports,
                                            &module.source_file,
                                        )?;
                                        method_bounds[idx].push(resolved);
                                        bj += 1;
                                    }
                                }
                            }
                        }
                        wi += 1;
                    }
                    traits.entries[entry_idx].methods[k].param_types = filled_params;
                    traits.entries[entry_idx].methods[k].return_type = filled_return;
                    traits.entries[entry_idx].methods[k].receiver_shape = receiver_shape;
                    traits.entries[entry_idx].methods[k].type_param_bounds = method_bounds;
                    k += 1;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_trait_methods(m, path, root_crate_name, traits, structs, enums, aliases, reexports)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(_) => {}
            Item::Use(_) => {}
            Item::TypeAlias(_) => {}
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
            Item::TypeAlias(_) => {}
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
            Item::TypeAlias(_) => {}
        }
        i += 1;
    }
}

// Second-pass: resolve each variant's payload types now that both struct
// and enum names are known. Mirrors `resolve_struct_fields`.
pub(super) fn resolve_enum_variants(
    module: &Module,
    path: &mut Vec<String>,
    root_crate_name: &str,
    table: &mut EnumTable,
    structs: &StructTable,
    aliases: &AliasTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = root_crate_name;
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
                                    aliases,
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
                                    aliases,
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
                resolve_enum_variants(m, path, root_crate_name, table, structs, aliases, reexports)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Struct(_) => {}
            Item::Impl(_) => {}
            Item::Trait(_) => {}
            Item::Use(_) => {}
            Item::TypeAlias(_) => {}
        }
        i += 1;
    }
    Ok(())
}

pub(super) fn resolve_struct_fields(
    module: &Module,
    path: &mut Vec<String>,
    root_crate_name: &str,
    table: &mut StructTable,
    enums: &EnumTable,
    aliases: &AliasTable,
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
                let crate_root: &str = root_crate_name;
    let use_scope = module_use_entries(module, crate_root);
                let mut k = 0;
                while k < sd.fields.len() {
                    let rt = resolve_type(
                        &sd.fields[k].ty,
                        path,
                        table,
                        enums,
                        aliases,
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
                resolve_struct_fields(m, path, root_crate_name, table, enums, aliases, reexports)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Enum(_) => {}
            Item::Impl(_) => {}
            Item::Trait(_) => {}
            Item::Use(_) => {}
            Item::TypeAlias(_) => {}
        }
        i += 1;
    }
    Ok(())
}

pub(super) fn collect_funcs(
    module: &Module,
    path: &mut Vec<String>,
    root_crate_name: &str,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
    structs: &StructTable,
    enums: &EnumTable,
    aliases: &AliasTable,
    traits: &mut TraitTable,
    reexports: &ReExportTable,
) -> Result<(), Error> {
    let crate_root: &str = root_crate_name;
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
                    aliases,
                    traits,
                    &use_scope,
                    reexports,
                    &module.source_file,
                )?;
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_funcs(m, path, root_crate_name, funcs, next_idx, structs, enums, aliases, traits, reexports)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(ib) => {
                let target_rt = resolve_impl_target(ib, path, structs, enums, aliases, &use_scope, reexports, &module.source_file)?;
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
                // Merge impl-level where-clause predicates into
                // `impl_type_param_bounds`. Param-LHS predicates
                // (`where T: Bound` where T is an impl-level
                // type-param) are equivalent to the inline
                // `<T: Bound>` form, so this just appends. Complex-
                // LHS predicates and lifetime predicates pass
                // through (parsed but not yet enforced).
                let mut wi = 0;
                while wi < ib.where_clause.len() {
                    if let crate::ast::WherePredicate::Type {
                        lhs, bounds, ..
                    } = &ib.where_clause[wi]
                    {
                        let lhs_rt = resolve_type(
                            lhs,
                            path,
                            structs,
                            enums,
                            aliases,
                            Some(&target_rt),
                            &impl_type_params,
                            &use_scope,
                            reexports,
                            &module.source_file,
                        )?;
                        if let RType::Param(name) = &lhs_rt {
                            let mut idx: Option<usize> = None;
                            let mut k = 0;
                            while k < impl_type_params.len() {
                                if &impl_type_params[k] == name {
                                    idx = Some(k);
                                    break;
                                }
                                k += 1;
                            }
                            if let Some(idx) = idx {
                                let mut bj = 0;
                                while bj < bounds.len() {
                                    let resolved = resolve_trait_path(
                                        &bounds[bj].path,
                                        path,
                                        traits,
                                        &use_scope,
                                        reexports,
                                        &module.source_file,
                                    )?;
                                    impl_type_param_bounds[idx].push(resolved);
                                    bj += 1;
                                }
                            }
                        }
                    }
                    wi += 1;
                }
                let trait_impl_idx_for_methods: Option<usize> =
                    if let Some(trait_path_node) = &ib.trait_path {
                        let (trait_full, trait_args) = resolve_trait_ref(
                            trait_path_node,
                            path,
                            structs,
                            enums,
                            aliases,
                            Some(&target_rt),
                            &impl_type_params,
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
                            aliases,
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
                            trait_args,
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
                // Whether this trait impl needs per-row disambiguation:
                // a trait with positional type-params can have
                // multiple impls on the same target (`impl Add<u32>
                // for Foo` + `impl Add<u64> for Foo`), and their
                // methods would collide at `[…, Foo, add]`. When the
                // trait declares trait-level type-params, append the
                // impl row idx to the prefix.
                let trait_is_generic = ib.trait_path.is_some()
                    && trait_impl_idx_for_methods.map_or(false, |idx| {
                        !traits.impls[idx].trait_args.is_empty()
                    });
                let mut method_prefix = path.clone();
                if let Some(name) = &target_name_for_prefix {
                    method_prefix.push(name.clone());
                    if trait_is_generic {
                        if let Some(idx) = trait_impl_idx_for_methods {
                            method_prefix.push(format!("__trait_impl_{}", idx));
                        }
                    }
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
                        aliases,
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
                    let (trait_full, trait_args) = resolve_trait_ref(
                        trait_path_node,
                        path,
                        structs,
                        enums,
                        aliases,
                        Some(&target_rt),
                        &impl_type_params,
                        traits,
                        &use_scope,
                        reexports,
                        &module.source_file,
                    )?;
                    validate_trait_impl_signatures(
                        ib,
                        &trait_full,
                        &trait_args,
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
            Item::TypeAlias(_) => {}
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
// Resolve a trait path *with* its positional type-args. The path's
// last segment carries the args (per `PathSegment.args` convention,
// shared with struct/enum/turbofish). Returns the canonical trait
// path plus the resolved RType for each arg. Validates arity against
// the trait's declared `trait_type_params`.
// Walk an `RType`, replacing every `AssocProj` whose `trait_path`
// is empty with the trait in `current_trait`'s supertrait closure
// that actually declares `name`. Falls back to `current_trait` if no
// member of the closure declares `name` (typeck will then surface a
// proper "no such assoc" error downstream rather than silently
// resolving to a wrong trait).
fn fill_assoc_trait_path_via_closure(
    rt: &RType,
    current_trait: &Vec<String>,
    traits: &TraitTable,
) -> RType {
    let recurse = |inner: &RType| fill_assoc_trait_path_via_closure(inner, current_trait, traits);
    match rt {
        RType::AssocProj { base, trait_path, name } => {
            let new_base = recurse(base);
            let resolved_tp = if trait_path.is_empty() {
                let closure = supertrait_closure(current_trait, traits);
                let mut found: Option<Vec<String>> = None;
                let mut i = 0;
                while i < closure.len() {
                    if let Some(t) = trait_lookup(traits, &closure[i]) {
                        if t.assoc_types.iter().any(|a| a == name) {
                            found = Some(closure[i].clone());
                            break;
                        }
                    }
                    i += 1;
                }
                found.unwrap_or_else(|| current_trait.clone())
            } else {
                trait_path.clone()
            };
            RType::AssocProj {
                base: Box::new(new_base),
                trait_path: resolved_tp,
                name: name.clone(),
            }
        }
        RType::Struct { path, type_args, lifetime_args } => {
            let mut new_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                new_args.push(recurse(&type_args[i]));
                i += 1;
            }
            RType::Struct { path: path.clone(), type_args: new_args, lifetime_args: lifetime_args.clone() }
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let mut new_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                new_args.push(recurse(&type_args[i]));
                i += 1;
            }
            RType::Enum { path: path.clone(), type_args: new_args, lifetime_args: lifetime_args.clone() }
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
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(recurse(inner)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(recurse(inner)),
            mutable: *mutable,
        },
        RType::Slice(inner) => RType::Slice(Box::new(recurse(inner))),
        RType::Param(_)
        | RType::Int(_)
        | RType::Bool
        | RType::Str
        | RType::Never
        | RType::Char
        | RType::Opaque { .. } => rt.clone(),
    }
}

// Resolve `<target as trait>::Name` projections against *this* impl's
// own bindings. Used by impl-validation to turn a substituted trait
// method signature like `<Foo as Mix>::Output` into the concrete
// type the impl assigns to `Output` — without going through the
// global impl table, which can return multiple matches when several
// `impl Mix<X> for Foo` rows exist with different bindings (the
// trait_args differ but the projection has no place to record them).
fn resolve_self_assoc_via_impl(
    rt: &RType,
    trait_full: &Vec<String>,
    target_rt: &RType,
    impl_assoc_bindings_ast: &Vec<crate::ast::ImplAssocType>,
    traits: &TraitTable,
    ib: &crate::ast::ImplBlock,
    file: &str,
) -> Result<RType, Error> {
    // Find this impl's row in the table to read its resolved
    // assoc_type_bindings (already validated/resolved at registration)
    // and trait_args (so we can disambiguate among multiple Index<X>
    // impls when the projection points at a supertrait's assoc).
    let idx = match crate::typeck::find_trait_impl_idx_by_span(traits, file, &ib.span) {
        Some(i) => i,
        None => {
            let _ = impl_assoc_bindings_ast;
            return Ok(crate::typeck::concretize_assoc_proj(rt, traits));
        }
    };
    let bindings: Vec<(String, RType)> = traits.impls[idx].assoc_type_bindings.clone();
    let trait_args: Vec<RType> = traits.impls[idx].trait_args.clone();
    Ok(walk_resolve_self_proj(
        rt,
        trait_full,
        &trait_args,
        target_rt,
        &bindings,
        traits,
    ))
}

fn walk_resolve_self_proj(
    rt: &RType,
    trait_full: &Vec<String>,
    trait_args: &Vec<RType>,
    target_rt: &RType,
    bindings: &Vec<(String, RType)>,
    traits: &TraitTable,
) -> RType {
    let recurse = |r: &RType| {
        walk_resolve_self_proj(r, trait_full, trait_args, target_rt, bindings, traits)
    };
    match rt {
        RType::AssocProj { base, trait_path, name } => {
            let new_base = recurse(base);
            // Resolution priority for `<base as P>::name`:
            //   1. P matches this impl's trait → look in `bindings`.
            //   2. P is a supertrait of this impl's trait → walk that
            //      supertrait edge with this impl's trait_args
            //      substituted in, then `find_assoc_binding_with_args`
            //      to disambiguate among multiple impl rows of the
            //      supertrait that share the same target.
            //   3. Otherwise (cross-trait): defer to global lookup,
            //      which uses the projection's own trait_path.
            let trait_match_self = trait_path.is_empty() || trait_path == trait_full;
            let base_matches = rtype_eq(&new_base, target_rt);
            if trait_match_self && base_matches {
                let mut k = 0;
                while k < bindings.len() {
                    if bindings[k].0 == *name {
                        return recurse(&bindings[k].1);
                    }
                    k += 1;
                }
            }
            // Supertrait path (only meaningful when base matches —
            // otherwise the global lookup is the right fallback).
            if base_matches && !trait_path.is_empty() && trait_path != trait_full {
                if let Some(entry) = trait_lookup(traits, trait_full) {
                    let mut env: Vec<(String, RType)> = Vec::new();
                    env.push(("Self".to_string(), target_rt.clone()));
                    let mut tp = 0;
                    while tp < entry.trait_type_params.len() && tp < trait_args.len() {
                        env.push((
                            entry.trait_type_params[tp].clone(),
                            trait_args[tp].clone(),
                        ));
                        tp += 1;
                    }
                    let mut s = 0;
                    while s < entry.supertraits.len() {
                        let sup = &entry.supertraits[s];
                        if &sup.path != trait_path {
                            s += 1;
                            continue;
                        }
                        let sup_args: Vec<RType> = sup
                            .args
                            .iter()
                            .map(|a| substitute_rtype(a, &env))
                            .collect();
                        let found = crate::typeck::traits::find_assoc_binding_with_args(
                            traits,
                            target_rt,
                            &sup.path,
                            &Some(sup_args),
                            name,
                        );
                        if found.len() == 1 {
                            return recurse(&found[0]);
                        }
                        s += 1;
                    }
                }
            }
            // Cross-trait projection (explicit `<T as Trait>::Name`)
            // — defer to global lookup, which disambiguates by the
            // projection's own trait_path.
            let projected = RType::AssocProj {
                base: Box::new(new_base),
                trait_path: trait_path.clone(),
                name: name.clone(),
            };
            crate::typeck::concretize_assoc_proj(&projected, traits)
        }
        RType::Struct { path, type_args, lifetime_args } => {
            let new_args: Vec<RType> = type_args.iter().map(|a| recurse(a)).collect();
            RType::Struct { path: path.clone(), type_args: new_args, lifetime_args: lifetime_args.clone() }
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let new_args: Vec<RType> = type_args.iter().map(|a| recurse(a)).collect();
            RType::Enum { path: path.clone(), type_args: new_args, lifetime_args: lifetime_args.clone() }
        }
        RType::Tuple(elems) => {
            let new_elems: Vec<RType> = elems.iter().map(|e| recurse(e)).collect();
            RType::Tuple(new_elems)
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
        RType::Slice(inner) => RType::Slice(Box::new(recurse(inner))),
        RType::Param(_)
        | RType::Int(_)
        | RType::Bool
        | RType::Str
        | RType::Never
        | RType::Char
        | RType::Opaque { .. } => rt.clone(),
    }
}

// Walk an `RType`, replacing every `AssocProj` whose `trait_path` is
// empty with `default_trait_path`. Empty trait_paths are emitted by
// `resolve_type` when it sees `Self::Name` / `T::Name` (the parser
// can't tell which trait the assoc belongs to). Trait method
// signatures fill them with the trait being declared; impl method
// signatures fill them with the impl's trait.
fn fill_assoc_trait_path(rt: &RType, default_trait_path: &Vec<String>) -> RType {
    match rt {
        RType::AssocProj { base, trait_path, name } => {
            let new_base = fill_assoc_trait_path(base, default_trait_path);
            let tp = if trait_path.is_empty() {
                default_trait_path.clone()
            } else {
                trait_path.clone()
            };
            RType::AssocProj {
                base: Box::new(new_base),
                trait_path: tp,
                name: name.clone(),
            }
        }
        RType::Struct { path, type_args, lifetime_args } => {
            let mut new_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                new_args.push(fill_assoc_trait_path(&type_args[i], default_trait_path));
                i += 1;
            }
            RType::Struct { path: path.clone(), type_args: new_args, lifetime_args: lifetime_args.clone() }
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let mut new_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                new_args.push(fill_assoc_trait_path(&type_args[i], default_trait_path));
                i += 1;
            }
            RType::Enum { path: path.clone(), type_args: new_args, lifetime_args: lifetime_args.clone() }
        }
        RType::Tuple(elems) => {
            let mut new_elems: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                new_elems.push(fill_assoc_trait_path(&elems[i], default_trait_path));
                i += 1;
            }
            RType::Tuple(new_elems)
        }
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(fill_assoc_trait_path(inner, default_trait_path)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(fill_assoc_trait_path(inner, default_trait_path)),
            mutable: *mutable,
        },
        RType::Slice(inner) => {
            RType::Slice(Box::new(fill_assoc_trait_path(inner, default_trait_path)))
        }
        RType::Param(_)
        | RType::Int(_)
        | RType::Bool
        | RType::Str
        | RType::Never
        | RType::Char
        | RType::Opaque { .. } => rt.clone(),
    }
}

pub(super) fn resolve_trait_ref(
    p: &crate::ast::Path,
    current_module: &Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    aliases: &AliasTable,
    self_target: Option<&RType>,
    type_params: &Vec<String>,
    traits: &TraitTable,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Result<(Vec<String>, Vec<RType>), Error> {
    let trait_path = resolve_trait_path(p, current_module, traits, use_scope, reexports, file)?;
    let entry = trait_lookup(traits, &trait_path).expect("just resolved");
    let last = &p.segments[p.segments.len() - 1];
    let total_params = entry.trait_type_params.len();
    let provided = last.args.len();
    // Each trailing slot the user omitted must have a default; otherwise
    // it's an error.
    if provided > total_params {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "too many type arguments for trait `{}`: expected at most {}, got {}",
                place_to_string(&trait_path),
                total_params,
                provided
            ),
            span: p.span.copy(),
        });
    }
    let mut k = provided;
    while k < total_params {
        if entry.trait_type_param_defaults[k].is_none() {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "missing type argument for trait `{}`: parameter `{}` has no default",
                    place_to_string(&trait_path),
                    entry.trait_type_params[k]
                ),
                span: p.span.copy(),
            });
        }
        k += 1;
    }
    let mut trait_args: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < provided {
        trait_args.push(resolve_type(
            &last.args[i],
            current_module,
            structs,
            enums,
            aliases,
            self_target,
            type_params,
            use_scope,
            reexports,
            file,
        )?);
        i += 1;
    }
    // Fill defaults. Each default may reference earlier trait-args
    // (via `Param(name)` of an earlier slot) or `Self` (substituted by
    // the in-scope `self_target`, which is the implementing type at
    // an `impl Trait for Foo` site or the bound holder at `T: Trait`).
    // Build a substitution env from earlier slots' resolved args plus
    // a `Self` binding when self_target is provided.
    let mut k = provided;
    while k < total_params {
        let default_rt = entry.trait_type_param_defaults[k]
            .as_ref()
            .expect("checked above");
        let mut env: Vec<(String, RType)> = Vec::new();
        let mut j = 0;
        while j < trait_args.len() {
            env.push((entry.trait_type_params[j].clone(), trait_args[j].clone()));
            j += 1;
        }
        if let Some(st) = self_target {
            env.push(("Self".to_string(), st.clone()));
        }
        let resolved = substitute_rtype(default_rt, &env);
        trait_args.push(resolved);
        k += 1;
    }
    Ok((trait_path, trait_args))
}

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
pub(super) fn validate_trait_impl(
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
pub(super) fn validate_trait_impl_signatures(
    ib: &crate::ast::ImplBlock,
    trait_full: &Vec<String>,
    trait_args: &Vec<RType>,
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
        // Trait env: Self → impl_target, plus each trait-level
        // type-param (`Add<Rhs>`) → the impl's corresponding
        // trait_args slot.
        let mut trait_env: Vec<(String, RType)> =
            vec![("Self".to_string(), target_rt.clone())];
        let mut tta = 0;
        while tta < trait_entry.trait_type_params.len() && tta < trait_args.len() {
            trait_env.push((
                trait_entry.trait_type_params[tta].clone(),
                trait_args[tta].clone(),
            ));
            tta += 1;
        }
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
            // `Self::Item` projection now points at a concrete type
            // — resolve through *this impl's own bindings*. We can't
            // use the global `concretize_assoc_proj` here because it
            // walks every impl row matching `(trait_path, base)`
            // without filtering by `trait_args`, so two
            // `impl Mix<X> for Foo` rows (with different `Output`
            // bindings) make the lookup ambiguous and the validation
            // fails with a misleading "wrong return type" message.
            expected_param_types.push(resolve_self_assoc_via_impl(
                &subst, trait_full, target_rt, &ib.assoc_type_bindings, traits, ib, file,
            )?);
            p += 1;
        }
        let expected_return_type: Option<RType> = match trait_method.return_type.as_ref() {
            Some(rt) => Some({
                let subst = substitute_rtype(rt, &trait_env);
                resolve_self_assoc_via_impl(
                    &subst, trait_full, target_rt, &ib.assoc_type_bindings, traits, ib, file,
                )?
            }),
            None => None,
        };
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
            (Some(RType::Opaque { .. }), _) => {
                // Trait method declared `-> impl Trait` (RPITIT). The
                // impl can pick any concrete type that satisfies the
                // slot's bounds — and the impl's body-check has
                // already validated its own pin against its own
                // declared bounds. Skip strict signature equality.
            }
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
pub(super) fn register_trait_impl(
    ib: &crate::ast::ImplBlock,
    trait_full: &Vec<String>,
    trait_args: Vec<RType>,
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
            && trait_args.len() == traits.impls[i].trait_args.len()
            && trait_args.iter().zip(traits.impls[i].trait_args.iter()).all(|(a, b)| rtype_eq(a, b))
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
        trait_args,
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
    aliases: &AliasTable,
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
                    aliases,
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
        // Build the substitution env that maps the trait's type-params
        // (and `Self`) to this impl row's bindings. The supertrait's
        // declared `args` are written against the trait's vocabulary;
        // substitute through this env to get the obligation's args.
        let mut env: Vec<(String, RType)> = Vec::new();
        env.push(("Self".to_string(), row.target.clone()));
        let mut tp = 0;
        while tp < entry.trait_type_params.len() && tp < row.trait_args.len() {
            env.push((entry.trait_type_params[tp].clone(), row.trait_args[tp].clone()));
            tp += 1;
        }
        let mut s = 0;
        while s < entry.supertraits.len() {
            let sup = &entry.supertraits[s];
            let sup_args: Vec<RType> = sup
                .args
                .iter()
                .map(|a| substitute_rtype(a, &env))
                .collect();
            if crate::typeck::traits::solve_impl_in_ctx_with_args(
                &sup.path,
                &sup_args,
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
                        place_to_string(&sup.path),
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
    aliases: &AliasTable,
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
        aliases,
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

// Walk a `Type` AST and replace each `TypeKind::ImplTrait(bounds)`
// with a synth `Path("__rpit_<n>")` segment, allocating one synth
// name + one slot per occurrence (depth-first order). `synth_names`
// receives the synth names in the order the slots were allocated;
// `bounds_per_slot[n]` collects the AST trait bounds for slot N.
// The synth names get added to the `type_params` slice passed to
// `resolve_type` so they resolve as `RType::Param(synth)` — which a
// post-pass swaps for `RType::Opaque { fn_path, slot: n }`.
fn rewrite_rpit_in_type(
    ty: &crate::ast::Type,
    synth_names: &mut Vec<String>,
    bounds_per_slot: &mut Vec<Vec<crate::ast::TraitBound>>,
) -> crate::ast::Type {
    use crate::ast::{Path, PathSegment, Type, TypeKind};
    let kind = match &ty.kind {
        TypeKind::ImplTrait(bounds) => {
            let slot = synth_names.len() as u32;
            let name = format!("__rpit_slot_{}", slot);
            synth_names.push(name.clone());
            bounds_per_slot.push(bounds.clone());
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
                    new_args.push(rewrite_rpit_in_type(&s.args[j], synth_names, bounds_per_slot));
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
                new_elems.push(rewrite_rpit_in_type(&elems[i], synth_names, bounds_per_slot));
                i += 1;
            }
            TypeKind::Tuple(new_elems)
        }
        TypeKind::Ref { inner, mutable, lifetime } => TypeKind::Ref {
            inner: Box::new(rewrite_rpit_in_type(inner, synth_names, bounds_per_slot)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        TypeKind::RawPtr { inner, mutable } => TypeKind::RawPtr {
            inner: Box::new(rewrite_rpit_in_type(inner, synth_names, bounds_per_slot)),
            mutable: *mutable,
        },
        TypeKind::Slice(inner) => TypeKind::Slice(Box::new(
            rewrite_rpit_in_type(inner, synth_names, bounds_per_slot),
        )),
        TypeKind::SelfType => TypeKind::SelfType,
        TypeKind::Never => TypeKind::Never,
        TypeKind::Placeholder => TypeKind::Placeholder,
    };
    Type { kind, span: ty.span.copy() }
}

// Walk an `RType` and replace each `Param("__rpit_slot_N")` (a synth
// produced by `rewrite_rpit_in_type`) with the existential
// `Opaque { fn_path, slot: N }`. Other `Param` slots — real
// type-params — pass through unchanged.
fn substitute_rpit_synths_to_opaque(
    rt: &RType,
    synth_names: &Vec<String>,
    fn_path: &Vec<String>,
) -> RType {
    if synth_names.is_empty() {
        return rt.clone();
    }
    let recurse = |inner: &RType| substitute_rpit_synths_to_opaque(inner, synth_names, fn_path);
    match rt {
        RType::Param(name) => {
            let mut k = 0;
            while k < synth_names.len() {
                if synth_names[k] == *name {
                    return RType::Opaque {
                        fn_path: fn_path.clone(),
                        slot: k as u32,
                    };
                }
                k += 1;
            }
            rt.clone()
        }
        RType::Struct { path, type_args, lifetime_args } => RType::Struct {
            path: path.clone(),
            type_args: type_args.iter().map(&recurse).collect(),
            lifetime_args: lifetime_args.clone(),
        },
        RType::Enum { path, type_args, lifetime_args } => RType::Enum {
            path: path.clone(),
            type_args: type_args.iter().map(&recurse).collect(),
            lifetime_args: lifetime_args.clone(),
        },
        RType::Tuple(elems) => RType::Tuple(elems.iter().map(&recurse).collect()),
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(recurse(inner)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(recurse(inner)),
            mutable: *mutable,
        },
        RType::Slice(inner) => RType::Slice(Box::new(recurse(inner))),
        RType::AssocProj { base, trait_path, name } => RType::AssocProj {
            base: Box::new(recurse(base)),
            trait_path: trait_path.clone(),
            name: name.clone(),
        },
        RType::Bool
        | RType::Int(_)
        | RType::Str
        | RType::Never
        | RType::Char
        | RType::Opaque { .. } => rt.clone(),
    }
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
    aliases: &AliasTable,
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
            aliases,
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
    // Return type — RPIT-aware resolution. Each `impl Trait`
    // occurrence in the declared return type becomes an `RType::Opaque
    // { fn_path: full, slot: N }` where N is the slot index. The
    // bounds for each slot are resolved here too and stashed for the
    // FnSymbol/Template later in this function.
    let mut rpit_synth_names: Vec<String> = Vec::new();
    let mut rpit_slot_bounds_ast: Vec<Vec<crate::ast::TraitBound>> = Vec::new();
    let mut return_type = match &f.return_type {
        Some(ty) => Some({
            let rewritten = rewrite_rpit_in_type(
                ty,
                &mut rpit_synth_names,
                &mut rpit_slot_bounds_ast,
            );
            let mut extended_params = type_param_names.clone();
            let mut s = 0;
            while s < rpit_synth_names.len() {
                extended_params.push(rpit_synth_names[s].clone());
                s += 1;
            }
            let raw_rt = resolve_type(
                &rewritten,
                current_module,
                structs,
                enums,
                aliases,
                self_target,
                &extended_params,
                use_scope,
                reexports,
                source_file,
            )?;
            let with_opaques = substitute_rpit_synths_to_opaque(
                &raw_rt,
                &rpit_synth_names,
                &full,
            );
            crate::typeck::concretize_assoc_proj(&with_opaques, traits)
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
    // Parallel to `type_param_bounds`: positional trait-args at each
    // bound site. Stays empty for impl-level bound rows (impl bounds
    // don't carry args today). Used by bidirectional inference at
    // call sites to read `(P,)` out of an `F: Fn(P) -> R` bound.
    let mut type_param_bound_args: Vec<Vec<Vec<RType>>> = Vec::new();
    let mut i = 0;
    while i < impl_type_param_bounds.len() {
        let mut row: Vec<Vec<String>> = Vec::new();
        let mut row_args: Vec<Vec<RType>> = Vec::new();
        let mut j = 0;
        while j < impl_type_param_bounds[i].len() {
            row.push(impl_type_param_bounds[i][j].clone());
            row_args.push(Vec::new());
            j += 1;
        }
        type_param_bounds.push(row);
        type_param_bound_args.push(row_args);
        i += 1;
    }
    let mut i = 0;
    while i < f.type_params.len() {
        let mut row: Vec<Vec<String>> = Vec::new();
        let mut row_args: Vec<Vec<RType>> = Vec::new();
        let mut j = 0;
        while j < f.type_params[i].bounds.len() {
            let bound = &f.type_params[i].bounds[j];
            let (resolved_path, resolved_args) = resolve_trait_ref(
                &bound.path,
                current_module,
                structs,
                enums,
                aliases,
                self_target,
                &type_param_names,
                traits,
                use_scope,
                reexports,
                source_file,
            )?;
            // Validate Named lifetimes in the bound's resolved trait
            // args against the enclosing fn/impl's lifetime params
            // PLUS the bound's own `for<'a, 'b>` HRTB lifetimes. This
            // catches an undeclared `'a` in `fn f<F: Fn(&'a u32) ->
            // ...>` while accepting `fn f<F: for<'a> Fn(&'a u32) ->
            // ...>` (where `'a` is bound by the HRTB declaration).
            let mut bound_lifetime_scope = lifetime_param_names.clone();
            let mut h = 0;
            while h < bound.hrtb_lifetime_params.len() {
                bound_lifetime_scope.push(bound.hrtb_lifetime_params[h].name.clone());
                h += 1;
            }
            let mut a = 0;
            while a < resolved_args.len() {
                crate::typeck::validate_named_lifetimes(
                    &resolved_args[a],
                    &bound_lifetime_scope,
                    &bound.path.span,
                    source_file,
                )?;
                a += 1;
            }
            row.push(resolved_path);
            row_args.push(resolved_args);
            j += 1;
        }
        type_param_bounds.push(row);
        type_param_bound_args.push(row_args);
        i += 1;
    }
    // (Trait method bound inheritance is applied after the
    // bound_assoc table is built so the three parallel vectors stay
    // length-matched per slot. See below.)
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
                    aliases,
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
    // Inherit trait method bounds: for an impl method that
    // implements a trait, the corresponding trait method may
    // declare bounds (`fn x<T>() where T: Bar`) that the impl's
    // `T` automatically inherits. Append the trait method's
    // bounds to the matching impl-method slot in all three
    // parallel vectors (paths, args, assoc) so length invariants
    // hold downstream.
    if let Some(impl_idx) = trait_impl_idx {
        let trait_path_for_lookup = traits.impls[impl_idx].trait_path.clone();
        if let Some(trait_entry) = trait_lookup(traits, &trait_path_for_lookup) {
            let mut tm_idx: Option<usize> = None;
            let mut t = 0;
            while t < trait_entry.methods.len() {
                if trait_entry.methods[t].name == f.name {
                    tm_idx = Some(t);
                    break;
                }
                t += 1;
            }
            if let Some(tm_idx) = tm_idx {
                let tm_bounds = &trait_entry.methods[tm_idx].type_param_bounds;
                let trait_type_params_len = trait_entry.trait_type_params.len();
                let impl_off = impl_type_params.len();
                let mut mt = trait_type_params_len;
                while mt < tm_bounds.len() {
                    let dest_idx = impl_off + (mt - trait_type_params_len);
                    if dest_idx < type_param_bounds.len() {
                        let mut bj = 0;
                        while bj < tm_bounds[mt].len() {
                            type_param_bounds[dest_idx]
                                .push(tm_bounds[mt][bj].clone());
                            type_param_bound_args[dest_idx].push(Vec::new());
                            type_param_bound_assoc[dest_idx].push(Vec::new());
                            bj += 1;
                        }
                    }
                    mt += 1;
                }
            }
        }
    }
    // Where-clause predicates. Resolve each predicate's LHS:
    //   * `Param(name)` matching a known type-param → append the
    //     resolved bounds onto that param's bounds rows so they're
    //     indistinguishable from inline `<T: Bound>` bounds.
    //   * Anything else (`Vec<T>`, `&T`, `(T, U)`, …) → store on
    //     `where_predicates` for call-time enforcement after the
    //     type-param substitution is built.
    let mut where_predicates: Vec<crate::typeck::tables::WherePredResolved> = Vec::new();
    let mut lifetime_predicates: Vec<crate::typeck::tables::LifetimePredResolved> =
        Vec::new();
    let mut wi = 0;
    while wi < f.where_clause.len() {
        match &f.where_clause[wi] {
            crate::ast::WherePredicate::Lifetime { lhs, bounds, span } => {
                // Validate every named lifetime in the predicate is
                // declared in the enclosing fn/impl scope. Phase B
                // structural carry — borrowck doesn't yet consume
                // these as outlives obligations.
                if !lifetime_param_names.iter().any(|n| n == &lhs.name) {
                    return Err(crate::span::Error {
                        file: source_file.to_string(),
                        message: format!(
                            "undeclared lifetime `'{}` in where-clause",
                            lhs.name
                        ),
                        span: lhs.span.copy(),
                    });
                }
                let mut resolved_bounds: Vec<String> = Vec::new();
                let mut bi = 0;
                while bi < bounds.len() {
                    let b = &bounds[bi];
                    if !lifetime_param_names.iter().any(|n| n == &b.name) {
                        return Err(crate::span::Error {
                            file: source_file.to_string(),
                            message: format!(
                                "undeclared lifetime `'{}` in where-clause",
                                b.name
                            ),
                            span: b.span.copy(),
                        });
                    }
                    resolved_bounds.push(b.name.clone());
                    bi += 1;
                }
                lifetime_predicates.push(crate::typeck::tables::LifetimePredResolved {
                    lhs: lhs.name.clone(),
                    bounds: resolved_bounds,
                    span: span.copy(),
                });
                wi += 1;
                continue;
            }
            crate::ast::WherePredicate::Type { .. } => {}
        }
        let (pred_lhs, pred_bounds, pred_lifetime_bounds, pred_span) = match &f.where_clause[wi] {
            crate::ast::WherePredicate::Type { lhs, bounds, lifetime_bounds, span } => {
                (lhs, bounds, lifetime_bounds, span)
            }
            crate::ast::WherePredicate::Lifetime { .. } => unreachable!(),
        };
        let lhs_rt = resolve_type(
            pred_lhs,
            current_module,
            structs,
            enums,
            aliases,
            self_target,
            &type_param_names,
            use_scope,
            reexports,
            source_file,
        )?;
        // Validate trailing `+ 'lifetime` outlives bounds on the type
        // (`T: Trait + 'a`). Carry-only — borrowck doesn't enforce
        // outlives constraints yet, but we still verify each named
        // lifetime is in scope so the diagnostic catches typos.
        let mut k = 0;
        while k < pred_lifetime_bounds.len() {
            let lt = &pred_lifetime_bounds[k];
            if !lifetime_param_names.iter().any(|n| n == &lt.name) {
                return Err(crate::span::Error {
                    file: source_file.to_string(),
                    message: format!(
                        "undeclared lifetime `'{}` in where-clause",
                        lt.name
                    ),
                    span: lt.span.copy(),
                });
            }
            k += 1;
        }
        // Resolve every bound in this predicate to (path, args, assoc).
        let mut resolved_bounds: Vec<crate::typeck::tables::WhereBoundResolved> = Vec::new();
        let mut bi = 0;
        while bi < pred_bounds.len() {
            let b = &pred_bounds[bi];
            let (resolved_path, resolved_args) = resolve_trait_ref(
                &b.path,
                current_module,
                structs,
                enums,
                aliases,
                self_target,
                &type_param_names,
                traits,
                use_scope,
                reexports,
                source_file,
            )?;
            let mut bound_lifetime_scope = lifetime_param_names.clone();
            let mut h = 0;
            while h < b.hrtb_lifetime_params.len() {
                bound_lifetime_scope.push(b.hrtb_lifetime_params[h].name.clone());
                h += 1;
            }
            let mut a = 0;
            while a < resolved_args.len() {
                crate::typeck::validate_named_lifetimes(
                    &resolved_args[a],
                    &bound_lifetime_scope,
                    &b.path.span,
                    source_file,
                )?;
                a += 1;
            }
            let mut constraints: Vec<(String, RType)> = Vec::new();
            let mut c = 0;
            while c < b.assoc_constraints.len() {
                let ac = &b.assoc_constraints[c];
                let cty = resolve_type(
                    &ac.ty,
                    current_module,
                    structs,
                    enums,
                    aliases,
                    self_target,
                    &type_param_names,
                    use_scope,
                    reexports,
                    source_file,
                )?;
                constraints.push((ac.name.clone(), cty));
                c += 1;
            }
            resolved_bounds.push(crate::typeck::tables::WhereBoundResolved {
                trait_path: resolved_path,
                trait_args: resolved_args,
                assoc_constraints: constraints,
            });
            bi += 1;
        }
        // Param-LHS path: merge into the matching type-param's rows.
        let merged = match &lhs_rt {
            RType::Param(name) => {
                let mut idx: Option<usize> = None;
                let mut k = 0;
                while k < type_param_names.len() {
                    if type_param_names[k] == *name {
                        idx = Some(k);
                        break;
                    }
                    k += 1;
                }
                if let Some(idx) = idx {
                    let mut bk = 0;
                    while bk < resolved_bounds.len() {
                        let rb = &resolved_bounds[bk];
                        type_param_bounds[idx].push(rb.trait_path.clone());
                        type_param_bound_args[idx].push(rb.trait_args.clone());
                        type_param_bound_assoc[idx].push(rb.assoc_constraints.clone());
                        bk += 1;
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !merged {
            where_predicates.push(crate::typeck::tables::WherePredResolved {
                lhs: lhs_rt,
                bounds: resolved_bounds,
                span: pred_span.copy(),
            });
        }
        wi += 1;
    }
    // Resolve the bounds attached to each RPIT slot. Each slot's
    // bounds were collected as AST `TraitBound`s during the return-
    // type rewrite; resolve them here against the function's
    // type-param scope and stash on the FnSymbol/Template.
    let mut rpit_slots: Vec<crate::typeck::tables::RpitSlot> = Vec::new();
    let mut si = 0;
    while si < rpit_slot_bounds_ast.len() {
        let slot_bounds = &rpit_slot_bounds_ast[si];
        let mut resolved: Vec<crate::typeck::tables::RpitBound> = Vec::new();
        let mut bi = 0;
        while bi < slot_bounds.len() {
            let b = &slot_bounds[bi];
            let (resolved_path, resolved_args) = resolve_trait_ref(
                &b.path,
                current_module,
                structs,
                enums,
                aliases,
                self_target,
                &type_param_names,
                traits,
                use_scope,
                reexports,
                source_file,
            )?;
            let mut constraints: Vec<(String, RType)> = Vec::new();
            let mut c = 0;
            while c < b.assoc_constraints.len() {
                let ac = &b.assoc_constraints[c];
                let cty = resolve_type(
                    &ac.ty,
                    current_module,
                    structs,
                    enums,
                    aliases,
                    self_target,
                    &type_param_names,
                    use_scope,
                    reexports,
                    source_file,
                )?;
                constraints.push((ac.name.clone(), cty));
                c += 1;
            }
            resolved.push(crate::typeck::tables::RpitBound {
                trait_path: resolved_path,
                trait_args: resolved_args,
                assoc_constraints: constraints,
            });
            bi += 1;
        }
        rpit_slots.push(crate::typeck::tables::RpitSlot {
            bounds: resolved,
            pin: None,
        });
        si += 1;
    }
    if is_generic {
        funcs.templates.push(GenericTemplate {
            path: full,
            type_params: type_param_names,
            type_param_bounds,
            type_param_bound_args,
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
            pattern_ergo: Vec::new(),
            closures: Vec::new(),
            bare_closure_calls: Vec::new(),
            rpit_slots: rpit_slots.clone(),
            where_predicates,
            lifetime_predicates: lifetime_predicates.clone(),
        });
    } else {
        // Non-generic functions can't reference any type-param in a
        // where-clause LHS, so any leftover (non-merged) predicate is
        // a constraint on a fully-concrete type. Eagerly verify each
        // bound resolves at setup; that's the entire enforcement
        // story for them — no per-call recheck needed.
        let mut wp = 0;
        while wp < where_predicates.len() {
            let pred = &where_predicates[wp];
            let mut bk = 0;
            while bk < pred.bounds.len() {
                let b = &pred.bounds[bk];
                if crate::typeck::traits::solve_impl_with_args(
                    &b.trait_path,
                    &b.trait_args,
                    &pred.lhs,
                    traits,
                    0,
                )
                .is_none()
                {
                    return Err(crate::span::Error {
                        file: source_file.to_string(),
                        message: format!(
                            "where-clause predicate not satisfied: `{}: {}` has no matching impl",
                            crate::typeck::rtype_to_string(&pred.lhs),
                            crate::typeck::place_to_string(&b.trait_path),
                        ),
                        span: pred.span.copy(),
                    });
                }
                bk += 1;
            }
            wp += 1;
        }
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
            pattern_ergo: Vec::new(),
            closures: Vec::new(),
            bare_closure_calls: Vec::new(),
            rpit_slots: rpit_slots.clone(),
            lifetime_predicates: lifetime_predicates.clone(),
        });
        *next_idx += 1;
    }
    Ok(())
}
