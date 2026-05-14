// Object-safety check for `dyn Trait` coercions.
//
// A trait is object-safe iff every method (across the trait + its
// supertrait closure) satisfies all of:
//   1. The receiver is `&self` or `&mut self`. By-value `self` and
//      by-value mutable `mut self` are rejected.
//   2. No method-level type parameters. Each method-level generic
//      would need a separate vtable entry per monomorphization;
//      pocket-rust doesn't synthesize that.
//   3. `Self` doesn't appear in any argument or return position
//      outside the receiver. A `Self`-bearing arg would need a
//      witness of the concrete type at the call site, which the
//      type-erased object can't provide.
//
// The check fires lazily — only when typeck encounters an actual
// `&T → &dyn Trait` coercion or a method dispatch on a `&dyn Trait`
// receiver. Errors carry the offending method name + the rule that
// failed, so users see e.g.
//     cannot coerce to `dyn Foo`: method `take` takes `self` by value
//     cannot coerce to `dyn Foo`: method `map` has type parameters
//     cannot coerce to `dyn Foo`: method `eq` references `Self` outside receiver position

use super::tables::{TraitReceiverShape, TraitTable, trait_lookup};
use super::types::RType;
use crate::span::{Error, Span};

pub fn check_object_safety(
    trait_path: &Vec<String>,
    traits: &TraitTable,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    let entry = match trait_lookup(traits, trait_path) {
        Some(e) => e,
        None => return Err(Error {
            file: file.to_string(),
            message: format!(
                "cannot use `dyn {}`: trait not found",
                super::place_to_string(trait_path)
            ),
            span: span.copy(),
        }),
    };
    // Check the trait's own methods. Supertrait methods that fail
    // object-safety are silently skipped from the vtable (the
    // `dyn_vtable_methods` walker omits them), not errors here —
    // mirrors Rust's `where Self: Sized` carve-out without requiring
    // that syntax. The trait's *own* methods must all be safe though;
    // otherwise the vtable would be empty and dispatch impossible.
    let mut i = 0;
    while i < entry.methods.len() {
        let m = &entry.methods[i];
        check_method_obj_safe(m, trait_path, span, file)?;
        i += 1;
    }
    Ok(())
}

// Returns true iff `m` is individually object-safe (receiver shape OK,
// no method-level type-params, no Self outside receiver). Used by
// `dyn_vtable_methods` to skip unsafe supertrait methods without
// failing the whole coercion.
pub fn is_method_obj_safe(m: &super::tables::TraitMethodEntry) -> bool {
    match &m.receiver_shape {
        Some(TraitReceiverShape::BorrowImm) | Some(TraitReceiverShape::BorrowMut) => {}
        _ => return false,
    }
    if !m.type_params.is_empty() {
        return false;
    }
    let mut k = 1;
    while k < m.param_types.len() {
        if rtype_mentions_self(&m.param_types[k]) {
            return false;
        }
        k += 1;
    }
    if let Some(ret) = &m.return_type {
        if rtype_mentions_self(ret) {
            return false;
        }
    }
    true
}

// Walk a trait + its transitive supertrait closure, returning every
// object-safe method as `(declaring_trait_path, method_idx_in_that_trait, trait_args_for_that_trait)`
// in the order they appear in the vtable: direct methods first, then
// supertrait methods (BFS by trait). Skips object-unsafe supertrait
// methods (mirrors Rust's `where Self: Sized` carve-out).
//
// `trait_args` is the dyn type's positional trait-args for the named
// `trait_path`. For supertraits, the args are substituted via the
// supertrait edge's declared args (which reference the parent trait's
// type-params); we apply the same substitution.
pub fn dyn_vtable_methods(
    trait_path: &Vec<String>,
    trait_args: &Vec<RType>,
    traits: &TraitTable,
) -> Vec<(Vec<String>, usize, Vec<RType>)> {
    let mut out: Vec<(Vec<String>, usize, Vec<RType>)> = Vec::new();
    let entry = match trait_lookup(traits, trait_path) {
        Some(e) => e,
        None => return out,
    };
    let mut k = 0;
    while k < entry.methods.len() {
        out.push((trait_path.clone(), k, trait_args.clone()));
        k += 1;
    }
    // BFS over supertraits. For each supertrait edge, substitute the
    // edge's declared args (referencing the parent trait's type-params)
    // using the parent's `trait_args`.
    let env: Vec<(String, RType)> = entry
        .trait_type_params
        .iter()
        .zip(trait_args.iter())
        .map(|(n, t)| (n.clone(), t.clone()))
        .collect();
    let mut visited: Vec<Vec<String>> = vec![trait_path.clone()];
    let mut frontier: Vec<(Vec<String>, Vec<RType>)> = entry
        .supertraits
        .iter()
        .map(|s| {
            let substituted_args: Vec<RType> = s
                .args
                .iter()
                .map(|a| super::types::substitute_rtype(a, &env))
                .collect();
            (s.path.clone(), substituted_args)
        })
        .collect();
    while let Some((sp, sp_args)) = frontier.pop() {
        if visited.iter().any(|v| v == &sp) {
            continue;
        }
        visited.push(sp.clone());
        let sup_entry = match trait_lookup(traits, &sp) {
            Some(e) => e,
            None => continue,
        };
        let mut k = 0;
        while k < sup_entry.methods.len() {
            if is_method_obj_safe(&sup_entry.methods[k]) {
                out.push((sp.clone(), k, sp_args.clone()));
            }
            k += 1;
        }
        let sub_env: Vec<(String, RType)> = sup_entry
            .trait_type_params
            .iter()
            .zip(sp_args.iter())
            .map(|(n, t)| (n.clone(), t.clone()))
            .collect();
        for s in &sup_entry.supertraits {
            let substituted_args: Vec<RType> = s
                .args
                .iter()
                .map(|a| super::types::substitute_rtype(a, &sub_env))
                .collect();
            frontier.push((s.path.clone(), substituted_args));
        }
    }
    out
}

fn check_method_obj_safe(
    m: &super::tables::TraitMethodEntry,
    trait_path: &Vec<String>,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    let trait_name = super::place_to_string(trait_path);
    // Rule 1: receiver shape.
    match &m.receiver_shape {
        Some(TraitReceiverShape::BorrowImm) | Some(TraitReceiverShape::BorrowMut) => {}
        Some(TraitReceiverShape::Move) => {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "cannot coerce to `dyn {}`: method `{}` takes `self` by value",
                    trait_name, m.name
                ),
                span: span.copy(),
            });
        }
        None => {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "cannot coerce to `dyn {}`: associated function `{}` has no receiver",
                    trait_name, m.name
                ),
                span: span.copy(),
            });
        }
    }
    // Rule 2: no method-level type parameters.
    if !m.type_params.is_empty() {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "cannot coerce to `dyn {}`: method `{}` has type parameters",
                trait_name, m.name
            ),
            span: span.copy(),
        });
    }
    // Rule 3: `Self` only allowed in the receiver position. Walk
    // every non-receiver param + return type for `RType::Param("Self")`.
    let mut k = 1; // skip receiver (index 0)
    while k < m.param_types.len() {
        if rtype_mentions_self(&m.param_types[k]) {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "cannot coerce to `dyn {}`: method `{}` references `Self` outside receiver position",
                    trait_name, m.name
                ),
                span: span.copy(),
            });
        }
        k += 1;
    }
    if let Some(ret) = &m.return_type {
        if rtype_mentions_self(ret) {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "cannot coerce to `dyn {}`: method `{}` returns `Self`",
                    trait_name, m.name
                ),
                span: span.copy(),
            });
        }
    }
    Ok(())
}

fn rtype_mentions_self(t: &RType) -> bool {
    match t {
        RType::Param(name) => name == "Self",
        RType::Struct { type_args, .. } | RType::Enum { type_args, .. } => {
            type_args.iter().any(rtype_mentions_self)
        }
        RType::Tuple(elems) => elems.iter().any(rtype_mentions_self),
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } | RType::Slice(inner) => {
            rtype_mentions_self(inner)
        }
        // `Self::AssocName` is OK in dyn-method signatures: the dyn
        // type's bound list carries the assoc binding (`Output = R`),
        // so the concrete type is fully determined per-impl. The
        // dispatch substitutes through that binding before building
        // the call_indirect typeidx. We *don't* recurse into `base`
        // here (which would catch the inner `Self`).
        RType::AssocProj { .. } => false,
        RType::FnPtr { params, ret } => {
            params.iter().any(rtype_mentions_self) || rtype_mentions_self(ret)
        }
        RType::Bool
        | RType::Int(_)
        | RType::Str
        | RType::Never
        | RType::Char
        | RType::Opaque { .. }
        | RType::Dyn { .. } => false,
    }
}
