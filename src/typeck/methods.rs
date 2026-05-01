use super::{
    CheckCtx, InferType,
    LifetimeRepr, MethodCandidate, PendingMethodCall, PendingTraitDispatch,
    RType, ReceiverAdjust, TraitTable,
    TraitReceiverShape, check_expr,
    find_lifetime_source, find_method_candidates, infer_substitute, infer_to_string, is_mutable_place, num_trait_path, place_to_string, resolve_type,
    rtype_to_infer, supertrait_closure, trait_lookup, try_match_against_infer,
};
use crate::ast::Expr;
use crate::span::{Error, Span};

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
    // Find traits that declare this method by walking each bound's
    // supertrait closure.
    let bounds = if idx < ctx.type_param_bounds.len() {
        ctx.type_param_bounds[idx].clone()
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
    let matching_traits = collect_traits_declaring_method(ctx.traits, &bounds, &mc.method);
    dispatch_method_through_trait(
        ctx,
        mc,
        call_expr,
        InferType::Param(param_name.clone()),
        matching_traits,
        recv_through_mut_ref,
        param_name,
    )
}

// Returns the trait paths (post supertrait-closure, deduped) that
// declare `method`. Used by both the explicit bounded-param symbolic
// dispatch path and the num-lit-var implicit-bound path below.
fn collect_traits_declaring_method(
    traits: &TraitTable,
    starting_bounds: &Vec<Vec<String>>,
    method: &str,
) -> Vec<Vec<String>> {
    let mut matching_traits: Vec<Vec<String>> = Vec::new();
    let mut bi = 0;
    while bi < starting_bounds.len() {
        let closure = supertrait_closure(&starting_bounds[bi], traits);
        let mut ci = 0;
        while ci < closure.len() {
            if let Some(trait_entry) = trait_lookup(traits, &closure[ci]) {
                let mut mi = 0;
                while mi < trait_entry.methods.len() {
                    if trait_entry.methods[mi].name == method {
                        let already = matching_traits.iter().any(|t| t == &closure[ci]);
                        if !already {
                            matching_traits.push(closure[ci].clone());
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
    matching_traits
}

// Common dispatch logic for "method on a not-yet-pinned type": either
// `Param(T)` with `T: Bound` (the explicit bounded-symbolic path) or
// an unbound integer literal var with implicit `T: Num` (the num-lit
// path).
//
// `recv_self_infer`: the Self type to substitute in the trait method's
//   signature — `Param(name)` for the bounded path, `Var(v)` for the
//   num-lit path. Borrowck/codegen apply the appropriate adjust later.
// `display_name`: a name to mention in error messages (`"T"` for a
//   user-typed param, `"integer"` for a num-lit var).
fn dispatch_method_through_trait(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    recv_self_infer: InferType,
    matching_traits: Vec<Vec<String>>,
    recv_through_mut_ref: bool,
    display_name: String,
) -> Result<InferType, Error> {
    let param_name = display_name;
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
    let trait_param_types = trait_method.param_types.clone();
    let trait_return_type = trait_method.return_type.clone();
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
        recv_self_infer.clone(),
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
            inner: Box::new(recv_self_infer.clone()),
            mutable: true,
            lifetime: LifetimeRepr::Inferred(0),
        }
    } else {
        // The original recv may have been `T` (consume) or `&T` (shared
        // ref); we surface T in either case here. Codegen reapplies the
        // appropriate adjustment.
        recv_self_infer.clone()
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
            trait_path: trait_full.clone(),
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

pub(super) fn check_method_call(
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
    // Method on an unbound integer literal var (e.g. `30 + 12` or
    // `(-x).foo()` where the literal hasn't been pinned yet). Treat
    // it as if it had an implicit `T: Num` bound and dispatch through
    // the Num + supertrait closure (so `add`/`sub` find VecSpace,
    // `mul`/`div` find Num, etc.). The recv stays as the var; codegen
    // picks up the concrete type after body-end pinning.
    if let InferType::Var(v) = &resolved_recv {
        if ctx.subst.is_num_lit[*v as usize] {
            let num_path = num_trait_path();
            let matching = collect_traits_declaring_method(
                ctx.traits,
                &vec![num_path],
                &mc.method,
            );
            return dispatch_method_through_trait(
                ctx,
                mc,
                call_expr,
                InferType::Var(*v),
                matching,
                false,
                "integer".to_string(),
            );
        }
    }
    if let InferType::Ref { inner, mutable, .. } = &resolved_recv {
        if let InferType::Var(v) = inner.as_ref() {
            if ctx.subst.is_num_lit[*v as usize] {
                let num_path = num_trait_path();
                let matching = collect_traits_declaring_method(
                    ctx.traits,
                    &vec![num_path],
                    &mc.method,
                );
                return dispatch_method_through_trait(
                    ctx,
                    mc,
                    call_expr,
                    InferType::Var(*v),
                    matching,
                    *mutable,
                    "integer".to_string(),
                );
            }
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
                let peeled = (**inner).clone();
                (kind, resolved_recv.clone(), Some(peeled))
            }
            _ => (RecvShape::Owned, resolved_recv.clone(), None),
        };
    // Pull out struct_path + recv_type_args for downstream env-building
    // (only meaningful when the matched impl_target is struct-shaped).
    let struct_path: Vec<String> = match &resolved_recv {
        InferType::Struct { path, .. } => path.clone(),
        InferType::Ref { inner, .. } => match inner.as_ref() {
            InferType::Struct { path, .. } => path.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let _recv_type_args: Vec<InferType> = match &resolved_recv {
        InferType::Struct { type_args, .. } => type_args.clone(),
        InferType::Ref { inner, .. } => match inner.as_ref() {
            InferType::Struct { type_args, .. } => type_args.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut method_path = struct_path.clone();
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
                ctx.funcs.entries[*i].impl_target.clone()
            }
            MethodCandidate::Template(i) => {
                ctx.funcs.templates[*i].impl_target.clone()
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
                    ctx.funcs.entries[*i].param_types[0].clone()
                }
                MethodCandidate::Template(i) => {
                    ctx.funcs.templates[*i].param_types[0].clone()
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
                entry.param_types.clone(),
                entry.return_type.clone(),
                Vec::new(),
                entry.idx,
                entry.param_lifetimes.clone(),
                entry.ret_lifetime.clone(),
                false,
                0usize,
            )
        }
        MethodCandidate::Template(idx) => {
            let t = &ctx.funcs.templates[idx];
            (
                t.param_types.clone(),
                t.return_type.clone(),
                t.type_params.clone(),
                0u32,
                t.param_lifetimes.clone(),
                t.ret_lifetime.clone(),
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
                    found = Some(chosen_env[k].1.clone());
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
            v.push(env[i].1.clone());
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
        callee_path: method_path.clone(),
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
