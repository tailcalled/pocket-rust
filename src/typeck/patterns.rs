use super::{
    CheckCtx, InferType, LifetimeRepr, RType, RTypedField,
    VariantPayloadResolved, enum_lookup,
    infer_substitute, infer_to_rtype_for_check, infer_to_string, is_visible_from,
    lookup_variant_path, place_to_string, resolve_via_use_scopes, rtype_to_infer,
    segments_to_string, struct_lookup, struct_lookup_resolved, substitute_rtype,
    type_defining_module,
};
use crate::ast::{Path, Pattern};
use crate::span::{Error, Span};

// Type-check a pattern against `scrutinee_ty`, appending `(name, ty,
// span)` for every binding the pattern introduces. Recurses into
// sub-patterns. The final pattern type is `scrutinee_ty` itself
// (patterns are checked for compatibility, not unified to a different
// type).
pub(super) fn check_pattern(
    ctx: &mut CheckCtx,
    pattern: &Pattern,
    scrutinee_ty: &InferType,
    bindings: &mut Vec<(String, InferType, Span, bool)>,
) -> Result<(), Error> {
    use crate::ast::PatternKind;
    // Record the resolved scrutinee type at this pattern's NodeId so
    // codegen can look it up directly without re-inferring.
    ctx.expr_infer_types[pattern.id as usize] = Some(scrutinee_ty.clone());
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
                    inner: Box::new(scrutinee_ty.clone()),
                    mutable: *mutable,
                    lifetime: LifetimeRepr::Inferred(0),
                }
            } else {
                scrutinee_ty.clone()
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
            let tuple_ty = InferType::Tuple(elem_tys.clone());
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
                .map(|(n, t, s, m)| (n.clone(), t.clone(), s.copy(), *m))
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
                    .map(|(n, t, s, m)| (n.clone(), t.clone(), s.copy(), *m))
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
                    first_alt_bindings[k].1.clone(),
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
                scrutinee_ty.clone(),
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
        path: entry.path.clone(),
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
            let payload_types: Vec<RType> = types.clone();
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
                    ty: fs[k].ty.clone(),
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
        path: entry.path.clone(),
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
        let mut full = ctx.current_module.clone();
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
    let canon_path = entry.path.clone();
    let field_defs: Vec<RTypedField> = {
        let mut out: Vec<RTypedField> = Vec::new();
        let mut k = 0;
        while k < entry.fields.len() {
            out.push(RTypedField {
                name: entry.fields[k].name.clone(),
                name_span: entry.fields[k].name_span.copy(),
                ty: entry.fields[k].ty.clone(),
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
        path: canon_path.clone(),
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
pub(super) fn check_match_exhaustive(
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
                // Skip variants whose payload is uninhabited (e.g.
                // `Err(!)` in `Result<T, !>`) — they can never be
                // constructed, so the match doesn't need to cover
                // them. This is what lets `impl<T> Result<T, !> { fn
                // into_ok(self) -> T { match self { Ok(v) => v, } } }`
                // typecheck without an Err arm.
                if crate::typeck::is_variant_payload_uninhabited(
                    &entry.variants[v].payload,
                    &env,
                    ctx.structs,
                    ctx.enums,
                ) {
                    v += 1;
                    continue;
                }
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
