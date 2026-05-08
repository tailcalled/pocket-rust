use super::{EnumTable, RType, StructTable, VariantPayloadResolved};

// Per-parameter variance classification — populated by `compute_variance`
// over a struct's / enum's field types. Borrowck reads these at every
// value-flow boundary between two same-path types with different
// region (or generic-type) args, to decide whether to emit a one-way
// outlives constraint (Covariant) or to equate (Invariant).
//
// Pocket-rust has no contravariant positions today (no fn pointers, no
// trait-object input types), so the lattice degenerates to a chain:
// Covariant < Invariant. Variance only narrows; once Invariant, stays
// Invariant. When fn pointers land, add a `Contravariant` variant and
// extend the meet/compose tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variance {
    Covariant,
    Invariant,
}

// Compose two variances: the variance of `T` when `T` appears in a
// position whose own variance is `outer`, and is itself wrapped by
// something with variance `inner`. With only Cov/Inv: Inv ∘ anything =
// Inv; Cov ∘ anything = anything.
pub fn compose(outer: Variance, inner: Variance) -> Variance {
    match (outer, inner) {
        (Variance::Covariant, v) => v,
        (Variance::Invariant, _) => Variance::Invariant,
    }
}

// Meet (greatest lower bound) two variances. With Cov < Inv as the
// chain, `meet` is just Inv-if-either-is-Inv.
pub fn meet(a: Variance, b: Variance) -> Variance {
    match (a, b) {
        (Variance::Covariant, Variance::Covariant) => Variance::Covariant,
        _ => Variance::Invariant,
    }
}

// Iterate over all structs and enums, refining `type_param_variance`
// and `lifetime_param_variance` from each field's type. Initial state
// (set at entry construction) is all-Covariant; each field-walk
// narrows entries via `meet`. Fixpoint terminates because the lattice
// is a finite chain — each (entry, slot) can only flip Cov→Inv once.
pub fn compute_variance(structs: &mut StructTable, enums: &mut EnumTable) {
    loop {
        let mut changed = false;
        // Snapshot current variances so the walker reads a consistent
        // view while we mutate. Cheap because the vectors are small
        // and we only iterate to fixpoint.
        let struct_snap: Vec<(Vec<Variance>, Vec<Variance>)> = structs
            .entries
            .iter()
            .map(|e| (e.type_param_variance.clone(), e.lifetime_param_variance.clone()))
            .collect();
        let enum_snap: Vec<(Vec<Variance>, Vec<Variance>)> = enums
            .entries
            .iter()
            .map(|e| (e.type_param_variance.clone(), e.lifetime_param_variance.clone()))
            .collect();

        let mut s = 0;
        while s < structs.entries.len() {
            let entry = &structs.entries[s];
            let mut new_tp = entry.type_param_variance.clone();
            let mut new_lp = entry.lifetime_param_variance.clone();
            let tp_names = entry.type_params.clone();
            let lp_names = entry.lifetime_params.clone();
            let fields_clone: Vec<RType> = entry.fields.iter().map(|f| f.ty.clone()).collect();
            for fty in &fields_clone {
                walk(
                    fty,
                    Variance::Covariant,
                    &tp_names,
                    &lp_names,
                    &mut new_tp,
                    &mut new_lp,
                    structs,
                    enums,
                    &struct_snap,
                    &enum_snap,
                );
            }
            if new_tp != structs.entries[s].type_param_variance
                || new_lp != structs.entries[s].lifetime_param_variance
            {
                changed = true;
                structs.entries[s].type_param_variance = new_tp;
                structs.entries[s].lifetime_param_variance = new_lp;
            }
            s += 1;
        }

        let mut e = 0;
        while e < enums.entries.len() {
            let entry = &enums.entries[e];
            let mut new_tp = entry.type_param_variance.clone();
            let mut new_lp = entry.lifetime_param_variance.clone();
            let tp_names = entry.type_params.clone();
            let lp_names = entry.lifetime_params.clone();
            let payloads_clone: Vec<RType> = collect_variant_payload_types(&entry.variants);
            for fty in &payloads_clone {
                walk(
                    fty,
                    Variance::Covariant,
                    &tp_names,
                    &lp_names,
                    &mut new_tp,
                    &mut new_lp,
                    structs,
                    enums,
                    &struct_snap,
                    &enum_snap,
                );
            }
            if new_tp != enums.entries[e].type_param_variance
                || new_lp != enums.entries[e].lifetime_param_variance
            {
                changed = true;
                enums.entries[e].type_param_variance = new_tp;
                enums.entries[e].lifetime_param_variance = new_lp;
            }
            e += 1;
        }

        if !changed {
            break;
        }
    }
}

fn collect_variant_payload_types(variants: &Vec<crate::typeck::EnumVariantEntry>) -> Vec<RType> {
    let mut out: Vec<RType> = Vec::new();
    for v in variants {
        match &v.payload {
            VariantPayloadResolved::Unit => {}
            VariantPayloadResolved::Tuple(types) => {
                for t in types {
                    out.push(t.clone());
                }
            }
            VariantPayloadResolved::Struct(fields) => {
                for f in fields {
                    out.push(f.ty.clone());
                }
            }
        }
    }
    out
}

// Walk an RType in a position with variance `position` (the variance
// at which this slot of the enclosing type sits). For each occurrence
// of one of `tp_names` / `lp_names`, narrow the corresponding entry
// in `out_tp` / `out_lp`. Recurse with `compose(position, slot_var)`
// when descending into another type's slot whose own variance is
// `slot_var`.
fn walk(
    rt: &RType,
    position: Variance,
    tp_names: &Vec<String>,
    lp_names: &Vec<String>,
    out_tp: &mut Vec<Variance>,
    out_lp: &mut Vec<Variance>,
    structs: &StructTable,
    enums: &EnumTable,
    struct_snap: &Vec<(Vec<Variance>, Vec<Variance>)>,
    enum_snap: &Vec<(Vec<Variance>, Vec<Variance>)>,
) {
    match rt {
        RType::Param(name) => {
            // Direct mention of a type-param of the enclosing struct/enum:
            // narrow that param's variance to `meet(current, position)`.
            let mut i = 0;
            while i < tp_names.len() {
                if &tp_names[i] == name {
                    out_tp[i] = meet(out_tp[i], position);
                    return;
                }
                i += 1;
            }
            // A `Param` not in our own params (shouldn't happen for fields,
            // but be defensive). No effect.
        }
        RType::Ref { inner, lifetime, mutable } => {
            // Outer lifetime: covariant in `'a` regardless of mutability.
            // (`&'a T <: &'b T` and `&'a mut T <: &'b mut T` both want
            // `'a: 'b`.)
            narrow_lifetime(lifetime, position, lp_names, out_lp);
            // Inner T: covariant for `&T`, INVARIANT for `&mut T`.
            let inner_pos = if *mutable {
                compose(position, Variance::Invariant)
            } else {
                position
            };
            walk(
                inner,
                inner_pos,
                tp_names,
                lp_names,
                out_tp,
                out_lp,
                structs,
                enums,
                struct_snap,
                enum_snap,
            );
        }
        RType::RawPtr { inner, .. } => {
            // Raw pointers are invariant in their pointee — there's no
            // automatic deref-coercion for them, so neither covariance
            // nor contravariance applies.
            walk(
                inner,
                compose(position, Variance::Invariant),
                tp_names,
                lp_names,
                out_tp,
                out_lp,
                structs,
                enums,
                struct_snap,
                enum_snap,
            );
        }
        RType::Tuple(elems) => {
            for e in elems {
                walk(
                    e,
                    position,
                    tp_names,
                    lp_names,
                    out_tp,
                    out_lp,
                    structs,
                    enums,
                    struct_snap,
                    enum_snap,
                );
            }
        }
        RType::Slice(inner) => {
            // `[T]` is covariant in T (matches `[T; N]` and `Vec<T>`).
            walk(
                inner,
                position,
                tp_names,
                lp_names,
                out_tp,
                out_lp,
                structs,
                enums,
                struct_snap,
                enum_snap,
            );
        }
        RType::Struct { path, type_args, lifetime_args } => {
            // Look up the struct's variance vectors (from the snapshot
            // — reading current state would be circular for mutually-
            // recursive types). Compose each slot's variance with the
            // current position, then walk that slot's type-arg under
            // the composed variance.
            let snap_idx = structs.entries.iter().position(|e| &e.path == path);
            for (i, ta) in type_args.iter().enumerate() {
                let slot_var = match snap_idx {
                    Some(idx) => *struct_snap[idx].0.get(i).unwrap_or(&Variance::Invariant),
                    None => Variance::Invariant,
                };
                walk(
                    ta,
                    compose(position, slot_var),
                    tp_names,
                    lp_names,
                    out_tp,
                    out_lp,
                    structs,
                    enums,
                    struct_snap,
                    enum_snap,
                );
            }
            for (i, la) in lifetime_args.iter().enumerate() {
                let slot_var = match snap_idx {
                    Some(idx) => *struct_snap[idx].1.get(i).unwrap_or(&Variance::Invariant),
                    None => Variance::Invariant,
                };
                narrow_lifetime(la, compose(position, slot_var), lp_names, out_lp);
            }
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let snap_idx = enums.entries.iter().position(|e| &e.path == path);
            for (i, ta) in type_args.iter().enumerate() {
                let slot_var = match snap_idx {
                    Some(idx) => *enum_snap[idx].0.get(i).unwrap_or(&Variance::Invariant),
                    None => Variance::Invariant,
                };
                walk(
                    ta,
                    compose(position, slot_var),
                    tp_names,
                    lp_names,
                    out_tp,
                    out_lp,
                    structs,
                    enums,
                    struct_snap,
                    enum_snap,
                );
            }
            for (i, la) in lifetime_args.iter().enumerate() {
                let slot_var = match snap_idx {
                    Some(idx) => *enum_snap[idx].1.get(i).unwrap_or(&Variance::Invariant),
                    None => Variance::Invariant,
                };
                narrow_lifetime(la, compose(position, slot_var), lp_names, out_lp);
            }
        }
        RType::AssocProj { base, .. } => {
            // Associated-type projections are invariant: `<T as Trait>::Output`
            // appearing in a struct field makes T invariant (since the
            // projection's resolution depends on T's exact type).
            walk(
                base,
                compose(position, Variance::Invariant),
                tp_names,
                lp_names,
                out_tp,
                out_lp,
                structs,
                enums,
                struct_snap,
                enum_snap,
            );
        }
        RType::Bool
        | RType::Int(_)
        | RType::Char
        | RType::Str
        | RType::Never
        | RType::Opaque { .. } => {}
    }
}

fn narrow_lifetime(
    lt: &crate::typeck::LifetimeRepr,
    position: Variance,
    lp_names: &Vec<String>,
    out_lp: &mut Vec<Variance>,
) {
    if let crate::typeck::LifetimeRepr::Named(name) = lt {
        let mut i = 0;
        while i < lp_names.len() {
            if &lp_names[i] == name {
                out_lp[i] = meet(out_lp[i], position);
                return;
            }
            i += 1;
        }
    }
    // `Inferred(_)` lifetimes are anonymous within a single struct/enum
    // declaration's elided refs; they don't bind to any of the
    // outer's `lifetime_params`, so they don't contribute. Same for
    // `Named("static")`.
}
