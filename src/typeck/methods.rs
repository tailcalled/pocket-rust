use super::{
    CheckCtx, InferType,
    LifetimeRepr, MethodCandidate, PendingMethodCall, PendingTraitDispatch,
    RType, ReceiverAdjust, TraitTable,
    TraitReceiverShape, check_expr, check_place_expr,
    find_lifetime_source, find_method_candidates, infer_substitute, infer_to_string, is_mutable_place, numeric_lit_op_traits_for_method, place_to_string, resolve_type,
    rtype_to_infer, supertrait_closure, trait_lookup, try_match_against_infer,
};
use crate::ast::Expr;
use crate::span::Error;

// Walk a method-call receiver looking for a root `Var(name)`. Returns
// the binding name when the receiver is `Var` directly, a
// `FieldAccess` chain rooted at a Var, a `TupleIndex` chain rooted at
// a Var, or a `Deref` chain rooted at a Var. Returns None for
// anything else (literal call results, struct lits, etc.).
fn extract_root_var_name(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        crate::ast::ExprKind::Var(name) => Some(name.as_str()),
        crate::ast::ExprKind::FieldAccess(fa) => extract_root_var_name(&fa.base),
        crate::ast::ExprKind::TupleIndex { base, .. } => extract_root_var_name(base),
        crate::ast::ExprKind::Deref(inner) => extract_root_var_name(inner),
        _ => None,
    }
}

// Shape of a receiver passed through symbolic (Param-bound) dispatch:
// owned `T`, `&T`, or `&mut T`. Drives the recv-adjust derivation
// against the trait method's declared receiver shape.
#[derive(Clone, Copy)]
enum SymRecvShape {
    Owned,
    SharedRef,
    MutRef,
}

// Shape of a closure-typed receiver. Used to derive the `recv_adjust`
// for `f.call(...)` / `f.call_mut(...)` / `f.call_once(...)` against
// the corresponding Fn-family method's declared receiver shape.
#[derive(Clone, Copy)]
enum ClosureRecvKind {
    Owned,
    SharedRef,
    MutRef,
}

// True iff `path`'s last segment is a synthesized closure struct name
// (`__closure_<idx>`). Recognized by prefix because the closure-lowering
// pass uses an opaque counter — no other items use this prefix.
fn is_closure_struct_path(path: &Vec<String>) -> bool {
    path.last()
        .map(|s| s.starts_with("__closure_"))
        .unwrap_or(false)
}

// If `resolved` (or its inner ref pointee) is a synthesized closure
// struct type, return the struct's InferType plus how the original
// receiver was shaped. Returns None for non-closure receivers.
fn extract_closure_recv(resolved: &InferType) -> Option<(InferType, ClosureRecvKind)> {
    match resolved {
        InferType::Struct { path, .. } if is_closure_struct_path(path) => {
            Some((resolved.clone(), ClosureRecvKind::Owned))
        }
        InferType::Ref { inner, mutable, .. } => match inner.as_ref() {
            InferType::Struct { path, .. } if is_closure_struct_path(path) => {
                let kind = if *mutable {
                    ClosureRecvKind::MutRef
                } else {
                    ClosureRecvKind::SharedRef
                };
                Some(((**inner).clone(), kind))
            }
            _ => None,
        },
        _ => None,
    }
}

// Look up the closure's recorded signature (param InferTypes + return
// InferType) by matching the receiver's struct path against entries in
// the current function's `closure_records` and (for closures defined
// in previously-checked functions) `funcs.entries[*].closures` /
// `funcs.templates[*].closures`. Returns None if no match — the caller
// then falls through to ordinary dispatch which will surface a
// "no method" diagnostic.
fn lookup_closure_signature(
    recv: &InferType,
    ctx: &CheckCtx,
) -> Option<(Vec<InferType>, InferType)> {
    let target_path = match recv {
        InferType::Struct { path, .. } => path.clone(),
        _ => return None,
    };
    // First check the current function's pending records — covers the
    // common case `let f = |...|...; f.call(...)` where both the
    // closure and its call live in the same function body.
    let mut i = 0;
    while i < ctx.closure_records.len() {
        if let Some(pc) = &ctx.closure_records[i] {
            if pc.synthesized_struct_path == target_path {
                return Some((pc.param_types.clone(), pc.return_type.clone()));
            }
        }
        i += 1;
    }
    // Fall through to closures recorded on already-checked functions
    // (closures-as-args / cross-function use cases). These entries
    // store finalized RTypes, which we lift back into InferTypes.
    let mut e = 0;
    while e < ctx.funcs.entries.len() {
        let mut k = 0;
        while k < ctx.funcs.entries[e].closures.len() {
            if let Some(ci) = &ctx.funcs.entries[e].closures[k] {
                if ci.synthesized_struct_path == target_path {
                    let params: Vec<InferType> =
                        ci.param_types.iter().map(rtype_to_infer).collect();
                    let ret = rtype_to_infer(&ci.return_type);
                    return Some((params, ret));
                }
            }
            k += 1;
        }
        e += 1;
    }
    let mut t = 0;
    while t < ctx.funcs.templates.len() {
        let mut k = 0;
        while k < ctx.funcs.templates[t].closures.len() {
            if let Some(ci) = &ctx.funcs.templates[t].closures[k] {
                if ci.synthesized_struct_path == target_path {
                    let params: Vec<InferType> =
                        ci.param_types.iter().map(rtype_to_infer).collect();
                    let ret = rtype_to_infer(&ci.return_type);
                    return Some((params, ret));
                }
            }
            k += 1;
        }
        t += 1;
    }
    None
}

// Closure-call dispatch. Already-validated: `mc.method` is one of
// `call` / `call_mut` / `call_once` and the receiver is a synthesized
// closure struct. Manually checks args against the closure's param
// tuple (Tuple of recorded param_types), populates a deferred
// trait_dispatch on the MethodResolution so codegen resolves the
// synth impl at emit time, and returns the closure's recorded return
// type directly (avoids surfacing `Self::Output` as an unresolved
// AssocProj at the call site, which would prevent inference from
// flowing through the call's result).
fn check_closure_method_call(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    recv_struct_infer: InferType,
    recv_kind: ClosureRecvKind,
    closure_param_types: Vec<InferType>,
    closure_return_type: InferType,
) -> Result<InferType, Error> {
    if mc.args.len() != 1 {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected 1 (an args tuple), got {}",
                mc.method,
                mc.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let expected_args_tuple = InferType::Tuple(closure_param_types);
    let arg_ty = check_expr(ctx, &mc.args[0])?;
    ctx.subst.coerce(
        &arg_ty,
        &expected_args_tuple,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bounds,
        &mc.args[0].span,
        ctx.current_file,
    )?;
    let trait_path: Vec<String> = vec![
        "std".to_string(),
        "ops".to_string(),
        match mc.method.as_str() {
            "call" => "Fn".to_string(),
            "call_mut" => "FnMut".to_string(),
            "call_once" => "FnOnce".to_string(),
            _ => unreachable!(),
        },
    ];
    let recv_adjust = match (mc.method.as_str(), recv_kind) {
        ("call", ClosureRecvKind::Owned) => ReceiverAdjust::BorrowImm,
        ("call", ClosureRecvKind::SharedRef) | ("call", ClosureRecvKind::MutRef) => {
            ReceiverAdjust::ByRef
        }
        ("call_mut", ClosureRecvKind::Owned) => ReceiverAdjust::BorrowMut,
        ("call_mut", ClosureRecvKind::MutRef) => ReceiverAdjust::ByRef,
        ("call_mut", ClosureRecvKind::SharedRef) => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message:
                    "cannot call `&mut self` method `call_mut` on a `&_` closure receiver"
                        .to_string(),
                span: mc.method_span.copy(),
            });
        }
        ("call_once", ClosureRecvKind::Owned) => ReceiverAdjust::Move,
        ("call_once", ClosureRecvKind::SharedRef)
        | ("call_once", ClosureRecvKind::MutRef) => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "cannot move out of borrow to call `call_once`".to_string(),
                span: mc.method_span.copy(),
            });
        }
        _ => unreachable!(),
    };
    ctx.method_resolutions[call_expr.id as usize] = Some(PendingMethodCall {
        callee_idx: 0,
        callee_path: Vec::new(),
        recv_adjust,
        ret_borrows_receiver: false,
        template_idx: None,
        type_arg_infers: Vec::new(),
        trait_dispatch: Some(PendingTraitDispatch {
            trait_path,
            trait_arg_infers: vec![expected_args_tuple],
            method_name: mc.method.clone(),
            recv_type_infer: recv_struct_infer,
            dispatch_span: call_expr.span.copy(),
        }),
    });
    // The closure's return type may still be an unresolved
    // `AssocProj` (e.g. `<?int as Add>::Output` for an unannotated
    // `|x| x + 1` body). Substitute through current bindings, then
    // concretize so the caller sees a concrete type and downstream
    // inference can proceed.
    let substituted = ctx.subst.substitute(&closure_return_type);
    Ok(crate::typeck::infer_concretize_assoc_proj(
        &substituted,
        ctx.traits,
        ctx.type_params,
        ctx.type_param_bound_assoc,
    ))
}

fn check_method_call_symbolic(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    param_name: String,
    recv_shape: SymRecvShape,
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
        recv_shape,
        param_name,
    )
}

// Method dispatch on an RPIT existential receiver. The slot's
// bounds (FnSymbol/Template.rpit_slots[slot].bounds) play the role
// `ctx.type_param_bounds[idx]` plays for `Param(T)` receivers —
// they list the traits the opaque type definitely impls. Walk the
// supertrait closure of each bound, find which declare `method`,
// and dispatch through that trait.
fn check_method_call_opaque(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    fn_path: Vec<String>,
    slot: u32,
    recv_shape: SymRecvShape,
) -> Result<InferType, Error> {
    // Extract the slot's bound paths (just paths — args/assoc are
    // matched at impl resolution time, not here).
    let bounds: Vec<Vec<String>> = {
        let mut out: Vec<Vec<String>> = Vec::new();
        if let Some(e) = ctx.funcs.entries.iter().find(|e| e.path == fn_path) {
            if let Some(s) = e.rpit_slots.get(slot as usize) {
                for b in &s.bounds {
                    out.push(b.trait_path.clone());
                }
            }
        } else if let Some(t) = ctx.funcs.templates.iter().find(|t| t.path == fn_path) {
            if let Some(s) = t.rpit_slots.get(slot as usize) {
                for b in &s.bounds {
                    out.push(b.trait_path.clone());
                }
            }
        }
        out
    };
    let matching_traits = collect_traits_declaring_method(ctx.traits, &bounds, &mc.method);
    let display_name = format!(
        "impl <{}#{}>",
        crate::typeck::place_to_string(&fn_path),
        slot
    );
    dispatch_method_through_trait(
        ctx,
        mc,
        call_expr,
        InferType::Opaque { fn_path, slot },
        matching_traits,
        recv_shape,
        display_name,
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

// Common dispatch logic for "method on a type whose impl can't be
// picked here-and-now": either `Param(T)` with `T: Bound` (the
// explicit bounded-symbolic path), an unbound integer literal var with
// implicit `T: Num` (the num-lit path), or a concrete recv where
// multiple impls of the same generic trait match (the deferred-dispatch
// path for `Foo{}.mix(0)` with multiple `impl Mix<X> for Foo` rows).
// In every case the trait_args are resolved later via inference, then
// `solve_impl_with_args` picks the actual impl at codegen / mono time.
//
// `recv_self_infer`: the Self type to substitute in the trait method's
//   signature — `Param(name)` for the bounded path, `Var(v)` for the
//   num-lit path, the concrete recv InferType for the deferred path.
//   Borrowck/codegen apply the appropriate adjust later.
// `display_name`: a name to mention in error messages (`"T"` for a
//   user-typed param, `"integer"` for a num-lit var).
fn dispatch_method_through_trait(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    recv_self_infer: InferType,
    matching_traits: Vec<Vec<String>>,
    recv_shape: SymRecvShape,
    display_name: String,
) -> Result<InferType, Error> {
    let recv_through_mut_ref = matches!(recv_shape, SymRecvShape::MutRef);
    let recv_through_shared_ref = matches!(recv_shape, SymRecvShape::SharedRef);
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
    // Trait-level type-params (e.g. `Rhs` in `trait Mix<Rhs>`). Each gets
    // a fresh inference var so usage-driven unification can pin them; the
    // resolved values land on `PendingTraitDispatch.trait_arg_infers` and
    // are converted to RType at body finalize.
    let trait_type_params: Vec<String> = trait_entry.trait_type_params.clone();
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
    // Allocate fresh inference vars for trait-level type-params and
    // record them on the dispatch (so codegen can call
    // `solve_impl_with_args` with their finalized RTypes).
    let mut trait_arg_infers: Vec<InferType> = Vec::new();
    let mut tap = 0;
    while tap < trait_type_params.len() {
        let v = ctx.subst.fresh_var();
        method_subst.push((trait_type_params[tap].clone(), InferType::Var(v)));
        trait_arg_infers.push(InferType::Var(v));
        tap += 1;
    }
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
            let user_infer = crate::typeck::resolve_type_to_infer(ctx, &mc.turbofish_args[t])?;
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
        ctx.subst.coerce(
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
            if recv_through_shared_ref {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of `&{}` to call `{}` (which takes `self` by value)",
                        param_name, mc.method
                    ),
                    span: mc.method_span.copy(),
                });
            }
            ReceiverAdjust::Move
        }
        Some(TraitReceiverShape::BorrowImm) => {
            if recv_through_mut_ref || recv_through_shared_ref {
                // Recv is already a ref — pass it as-is, no autoref.
                ReceiverAdjust::ByRef
            } else {
                // Owned recv → take its address.
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
            trait_arg_infers,
            method_name: mc.method.clone(),
            recv_type_infer: recv_for_storage,
            dispatch_span: mc.method_span.copy(),
        }),
    });
    // Return type comes from the trait method's declared signature with
    // Self + method-level type-params substituted. Tail-less methods
    // return `()`.
    let _ = call_expr;
    let infer = match &trait_return_type {
        Some(ret_rt) => {
            let raw = infer_substitute(&rtype_to_infer(ret_rt), &method_subst);
            crate::typeck::infer_concretize_assoc_proj(
                &raw,
                ctx.traits,
                ctx.type_params,
                ctx.type_param_bound_assoc,
            )
        }
        None => InferType::Tuple(Vec::new()),
    };
    Ok(infer)
}

pub(super) fn check_method_call(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
) -> Result<InferType, Error> {
    // Type-check the receiver in *place* mode. The method-dispatch
    // pipeline below decides whether the receiver gets autoref'd
    // (`BorrowImm`/`BorrowMut`), reborrowed (`ByRef`), or moved
    // (`Move`); only the Move case actually consumes the place. The
    // value-position "move out of borrow" check inside
    // `check_field_access` would fire too eagerly here — it'd reject
    // `self.value.clone()` for non-Copy `value` even though `.clone()`
    // takes `&self` and never moves. By using place-mode typing,
    // subsequent field-of-ref autoref'd receivers pass; borrowck still
    // catches genuine moves through-ref via its own move tracking on
    // `MethodCall { recv_adjust: Move, recv: ... }` sites.
    let recv_ty = check_place_expr(ctx, &mc.receiver)?;
    let resolved_recv = ctx.subst.substitute(&recv_ty);
    // Lazy projection: when recv is `AssocProj{base, …}` (typically
    // arising from a chained call like `(1 + 2).add(3)`), peel the
    // projection. With no global collapse heuristic in effect, the
    // result of the inner call stays wrapped as
    // `<Var as Add>::Output`, and the outer `.add(3)` would otherwise
    // hit the no-method path because dispatch can't match a method
    // on AssocProj. Three sub-cases:
    //   - Var base → unwrap and re-resolve (the inner Var is what
    //     governs dispatch; for num-lit Vars, the num-lit branch
    //     below picks the right trait).
    //   - Param base with a `T: Trait<Name = X>` constraint → resolve
    //     to X via `infer_concretize_assoc_proj` and continue.
    //   - Concrete base → resolve via `find_assoc_binding` and
    //     continue.
    // Wrapped in a small loop so a chain of nested AssocProjs gets
    // peeled in one pass.
    let mut peeled = resolved_recv.clone();
    loop {
        match &peeled {
            InferType::AssocProj { base, .. } => {
                let resolved = crate::typeck::infer_concretize_assoc_proj(
                    &peeled,
                    ctx.traits,
                    ctx.type_params,
                    ctx.type_param_bound_assoc,
                );
                if matches!(resolved, InferType::AssocProj { .. }) {
                    // `infer_concretize_assoc_proj` left it wrapped
                    // (base is Var, or no unique binding). For Var
                    // base, just unwrap to base — the num-lit branch
                    // below will dispatch through the trait. For
                    // anything else, leave it and let dispatch fail
                    // with a real "no method" message.
                    if matches!(base.as_ref(), InferType::Var(_)) {
                        peeled = (**base).clone();
                        break;
                    }
                    break;
                }
                peeled = resolved;
            }
            _ => break,
        }
    }
    let resolved_recv = peeled;
    // Closure call dispatch: when the method name is one of the Fn-
    // family methods AND the receiver type is a synthesized closure
    // struct (`__closure_<id>` registered by `check_closure`), bypass
    // the normal candidate lookup and dispatch through the recorded
    // `ClosureInfo`. The synthesized `Fn::call` impl is registered by
    // `closure_lower::lower` (running between typeck and borrowck), so
    // at the time `check_method_call` runs the impl isn't yet in
    // `traits.impls` — but we already know the closure's param/return
    // types from the side table populated by `check_closure`, so we
    // can build the `MethodResolution.trait_dispatch` directly.
    // `solve_impl_with_args` resolves the actual impl row at codegen
    // time once `closure_lower` has registered it.
    if matches!(mc.method.as_str(), "call" | "call_mut" | "call_once") {
        if let Some((recv_struct_infer, recv_kind)) =
            extract_closure_recv(&resolved_recv)
        {
            if let Some((param_types, return_type)) =
                lookup_closure_signature(&recv_struct_infer, ctx)
            {
                return check_closure_method_call(
                    ctx,
                    mc,
                    call_expr,
                    recv_struct_infer,
                    recv_kind,
                    param_types,
                    return_type,
                );
            }
        }
    }
    // T2: handle symbolic dispatch when recv is `Param(T)` — find the
    // method via T's trait bounds.
    if let InferType::Param(name) = &resolved_recv {
        return check_method_call_symbolic(
            ctx,
            mc,
            call_expr,
            name.clone(),
            SymRecvShape::Owned,
        );
    }
    if let InferType::Ref { inner, mutable, .. } = &resolved_recv {
        if let InferType::Param(name) = inner.as_ref() {
            let shape = if *mutable {
                SymRecvShape::MutRef
            } else {
                SymRecvShape::SharedRef
            };
            return check_method_call_symbolic(ctx, mc, call_expr, name.clone(), shape);
        }
        // Dyn-trait receiver dispatch: `&dyn Trait` / `&mut dyn Trait`.
        if let InferType::Dyn { bounds, .. } = inner.as_ref() {
            return check_dyn_method_call(
                ctx,
                mc,
                call_expr,
                bounds.clone(),
                *mutable,
            );
        }
    }
    // Box<dyn Trait> receiver dispatch: the box owns its T, so both
    // `&self` and `&mut self` methods are dispatchable. Codegen
    // extracts the box's two i32s (data ptr + vtable ptr) — same fat
    // shape as `&dyn Trait`.
    if let InferType::Struct { path, type_args, .. } = &resolved_recv {
        if crate::typeck::is_std_box_path(path) && type_args.len() == 1 {
            if let InferType::Dyn { bounds, .. } = &type_args[0] {
                return check_dyn_method_call(
                    ctx,
                    mc,
                    call_expr,
                    bounds.clone(),
                    true, // owned Box: any receiver shape (`&self`/`&mut self`) works
                );
            }
        }
    }
    // RPIT existential receiver: when the recv type is an `Opaque`,
    // method dispatch consults the slot's bounds — same shape as
    // Param-receiver symbolic dispatch but the bound list lives on
    // the FnSymbol's `rpit_slots[slot].bounds` rather than on
    // `ctx.type_param_bounds`. This makes forward-reference to RPIT
    // functions work: callers don't need the slot's pin to be set,
    // they only need the bounds.
    if let InferType::Opaque { fn_path, slot } = &resolved_recv {
        return check_method_call_opaque(
            ctx,
            mc,
            call_expr,
            fn_path.clone(),
            *slot,
            SymRecvShape::Owned,
        );
    }
    if let InferType::Ref { inner, mutable, .. } = &resolved_recv {
        if let InferType::Opaque { fn_path, slot } = inner.as_ref() {
            let shape = if *mutable {
                SymRecvShape::MutRef
            } else {
                SymRecvShape::SharedRef
            };
            return check_method_call_opaque(
                ctx,
                mc,
                call_expr,
                fn_path.clone(),
                *slot,
                shape,
            );
        }
    }
    // Method on an unbound integer literal var (e.g. `30 + 12` or
    // `(-x).foo()` where the literal hasn't been pinned yet). The var
    // can only resolve to a built-in integer type (literal overloading
    // is dropped), so we know exactly which traits are in play:
    // Add/Sub/Mul/Div/Rem/Neg + PartialEq/PartialOrd. We dispatch
    // symbolically through whichever of those declares the method; the
    // method's own signature drives arg checking, and the trait_args
    // (e.g. `Rhs` in `Add<Rhs>`) become fresh inference vars resolved
    // by usage. Codegen picks the impl after body-end pinning via
    // `solve_impl_with_args`.
    if let InferType::Var(v) = &resolved_recv {
        if ctx.subst.is_num_lit[*v as usize] {
            let matching = numeric_lit_op_traits_for_method(ctx.traits, &mc.method);
            return dispatch_method_through_trait(
                ctx,
                mc,
                call_expr,
                InferType::Var(*v),
                matching,
                SymRecvShape::Owned,
                "integer".to_string(),
            );
        }
    }
    if let InferType::Ref { inner, mutable, .. } = &resolved_recv {
        if let InferType::Var(v) = inner.as_ref() {
            if ctx.subst.is_num_lit[*v as usize] {
                let matching = numeric_lit_op_traits_for_method(ctx.traits, &mc.method);
                let shape = if *mutable {
                    SymRecvShape::MutRef
                } else {
                    SymRecvShape::SharedRef
                };
                return dispatch_method_through_trait(
                    ctx,
                    mc,
                    call_expr,
                    InferType::Var(*v),
                    matching,
                    shape,
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
    // Receiver-type-based dispatch (mirrors Rust's method-call
    // resolution). Each impl's method has an "effective receiver
    // type" Y = subst(method.params[0], Self → impl_target) — already
    // substituted at impl-method registration in setup.rs, so just the
    // raw `param_types[0]` of the impl method. We walk a candidate
    // self-type chain built from `recv_full` and at each level look
    // for impls whose Y unifies with the level. First level with at
    // least one match wins. Multi-match at the same level → ambiguity.
    //
    // Levels (in order):
    //   0 — recv_full as-is. If recv_full is a Ref → ByRef; else Move.
    //   1 — &recv_full (autoref imm) → BorrowImm.
    //   2 — &mut recv_full (autoref mut) → BorrowMut. Skipped when
    //       recv is not a mutable place.
    //
    // The deref level (`*recv_full`, when recv is a Ref) is *not*
    // implemented — pocket-rust doesn't currently support
    // autoderef-then-pass-by-value (it would require a Copy/move-out
    // analysis). Existing tests only exercise the three levels above.
    enum LevelKind {
        AsIs,
        AutorefImm,
        AutorefMut,
    }
    let mut levels: Vec<(InferType, ReceiverAdjust, LevelKind)> = Vec::new();
    let recv_is_ref = matches!(&recv_full, InferType::Ref { .. });
    let as_is_adjust = if recv_is_ref { ReceiverAdjust::ByRef } else { ReceiverAdjust::Move };
    levels.push((recv_full.clone(), as_is_adjust, LevelKind::AsIs));
    // Mutable→shared downgrade: when recv is `&mut T`, also try `&T`.
    // Mirrors Rust's auto-reborrow rule for method dispatch — lets a
    // `&self` method be called on a `&mut T` binding without an
    // explicit cast. ABI-wise this is a no-op (both refs are an i32
    // address), so `ReceiverAdjust::ByRef` (the "pass through as-is"
    // adjust) is the right pick.
    if let InferType::Ref { inner, mutable: true, .. } = &recv_full {
        levels.push((
            InferType::Ref {
                inner: inner.clone(),
                mutable: false,
                lifetime: crate::typeck::LifetimeRepr::Inferred(0),
            },
            ReceiverAdjust::ByRef,
            LevelKind::AsIs,
        ));
    }
    levels.push((
        InferType::Ref {
            inner: Box::new(recv_full.clone()),
            mutable: false,
            lifetime: crate::typeck::LifetimeRepr::Inferred(0),
        },
        ReceiverAdjust::BorrowImm,
        LevelKind::AutorefImm,
    ));
    if is_mutable_place(ctx, &mc.receiver) {
        levels.push((
            InferType::Ref {
                inner: Box::new(recv_full.clone()),
                mutable: true,
                lifetime: crate::typeck::LifetimeRepr::Inferred(0),
            },
            ReceiverAdjust::BorrowMut,
            LevelKind::AutorefMut,
        ));
    }
    let mut chosen: Option<(
        MethodCandidate,
        Vec<(String, InferType)>,
        Vec<(InferType, InferType)>,
        ReceiverAdjust,
    )> = None;
    for (level_ty, level_adjust, _level_kind) in &levels {
        let mut matches_at_level: Vec<(
            MethodCandidate,
            Vec<(String, InferType)>,
            Vec<(InferType, InferType)>,
        )> = Vec::new();
        for cand in &candidates {
            let (method_recv_param, impl_target_opt): (RType, Option<RType>) = match cand {
                MethodCandidate::Direct(i) => {
                    if ctx.funcs.entries[*i].param_types.is_empty() {
                        continue;
                    }
                    (
                        ctx.funcs.entries[*i].param_types[0].clone(),
                        ctx.funcs.entries[*i].impl_target.clone(),
                    )
                }
                MethodCandidate::Template(i) => {
                    if ctx.funcs.templates[*i].param_types.is_empty() {
                        continue;
                    }
                    (
                        ctx.funcs.templates[*i].param_types[0].clone(),
                        ctx.funcs.templates[*i].impl_target.clone(),
                    )
                }
            };
            let mut env_so_far: Vec<(String, InferType)> = Vec::new();
            let mut pending: Vec<(InferType, InferType)> = Vec::new();
            if !try_match_against_infer(
                &method_recv_param,
                level_ty,
                &ctx.subst,
                &mut env_so_far,
                &mut pending,
            ) {
                continue;
            }
            // Implicit `T: Sized` enforcement on impl-level type-params:
            // walk the impl_target to find which params appear outside
            // any Ref/RawPtr (those positions need a known size, e.g.
            // `impl<T> Trait for T` or `impl<T> Trait for Vec<T>`). For
            // any such param, the env binding must be Sized — otherwise
            // the impl doesn't actually cover the candidate type. Params
            // appearing only inside Ref/RawPtr (e.g. `impl<T> Copy for
            // &T`) are NOT subject to the Sized check here, mirroring
            // Rust's `impl<T: ?Sized> Copy for &T` opt-out.
            let mut sized_required: Vec<String> = Vec::new();
            if let Some(it) = &impl_target_opt {
                collect_sized_required_params(it, true, &mut sized_required);
            }
            let mut sized_ok = true;
            let mut k = 0;
            while k < env_so_far.len() {
                if sized_required.contains(&env_so_far[k].0) {
                    let resolved = ctx.subst.substitute(&env_so_far[k].1);
                    if !crate::typeck::is_sized_infer(&resolved) {
                        sized_ok = false;
                        break;
                    }
                }
                k += 1;
            }
            if !sized_ok {
                continue;
            }
            matches_at_level.push((*cand, env_so_far, pending));
        }
        if matches_at_level.is_empty() {
            continue;
        }
        if matches_at_level.len() > 1 {
            // Strategy (d): if every match is a method on a trait impl,
            // and they all come from impls of the same trait (with
            // differing trait_args), defer impl selection. We dispatch
            // through the trait method's signature with fresh inference
            // vars for each trait-arg slot; usage downstream pins them
            // and codegen runs `solve_impl_with_args` to pick the row.
            // This is what handles `Foo{}.mix(0)` for two
            // `impl Mix<X> for Foo` rows.
            let mut trait_paths: Vec<Vec<String>> = Vec::new();
            let mut all_have_trait = true;
            let mut ci = 0;
            while ci < matches_at_level.len() {
                let trait_idx = match &matches_at_level[ci].0 {
                    MethodCandidate::Direct(i) => ctx.funcs.entries[*i].trait_impl_idx,
                    MethodCandidate::Template(i) => ctx.funcs.templates[*i].trait_impl_idx,
                };
                match trait_idx {
                    Some(idx) => {
                        let path = ctx.traits.impls[idx].trait_path.clone();
                        if !trait_paths.iter().any(|p| *p == path) {
                            trait_paths.push(path);
                        }
                    }
                    None => {
                        all_have_trait = false;
                        break;
                    }
                }
                ci += 1;
            }
            if all_have_trait && trait_paths.len() == 1 {
                let trait_full = trait_paths.into_iter().next().unwrap();
                // Only defer when the trait carries type-params:
                // without them there's nothing for inference to pin
                // (e.g. `impl Trait for u32` + `impl<T> Trait for T`,
                // or two overlapping parametric patterns), and the
                // call-site truly is ambiguous. Generic-trait impls
                // (`impl Mix<u32> for Foo` + `impl Mix<i64> for Foo`)
                // do have a slot to thread through usage.
                let trait_has_params = trait_lookup(ctx.traits, &trait_full)
                    .map(|t| !t.trait_type_params.is_empty())
                    .unwrap_or(false);
                if trait_has_params {
                    let recv_shape = match &recv_full {
                        InferType::Ref { mutable: true, .. } => SymRecvShape::MutRef,
                        InferType::Ref { mutable: false, .. } => SymRecvShape::SharedRef,
                        _ => SymRecvShape::Owned,
                    };
                    let display = infer_to_string(&recv_full);
                    return dispatch_method_through_trait(
                        ctx,
                        mc,
                        call_expr,
                        recv_full.clone(),
                        vec![trait_full],
                        recv_shape,
                        display,
                    );
                }
            }
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
        let (cand, env_at, pending_at) = matches_at_level.into_iter().next().unwrap();
        chosen = Some((cand, env_at, pending_at, *level_adjust));
        break;
    }
    let (chosen_cand, mut chosen_env, chosen_pending, chosen_adjust) = match chosen {
        Some(c) => c,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("no method `{}` on `{}`", mc.method, infer_to_string(&recv_full)),
                span: mc.method_span.copy(),
            });
        }
    };
    // Suppress unused-warning for recv_kind / recv_peeled — kept for
    // potential future deref-level support but not consulted here.
    let _ = (&recv_kind, &recv_peeled);
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
                let user_infer = crate::typeck::resolve_type_to_infer(ctx, &mc.turbofish_args[k])?;
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
    // recv_adjust was already determined by which level of the
    // candidate-self-type chain matched.
    let recv_adjust = chosen_adjust;
    // Closure-mutation propagation: when the call's `recv_adjust` is
    // `BorrowMut` (the method takes `&mut self` — covers compound-
    // assign desugars `x += rhs` → `x.add_assign(rhs)` and explicit
    // `recv.method_taking_mut_self()`), and the recv's root is a
    // captured binding, upgrade the capture to RefMut. Drives the
    // FnMut-only synthesis for closures whose body mutates captures
    // through methods.
    if matches!(recv_adjust, ReceiverAdjust::BorrowMut) {
        if let Some(root_name) = extract_root_var_name(&mc.receiver) {
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
                crate::typeck::upgrade_capture_to_ref_mut(ctx, root_name, idx);
            }
        }
    }
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
        ctx.subst.coerce(
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

// `RecvShape` was used by the old pattern-tier dispatch's per-shape
// adjust derivation. Receiver-type-based dispatch determines the
// adjustment from which level of the candidate-self-type chain
// matched, so neither the shape enum nor the derive helpers are
// referenced any longer; kept as deletions in the diff.
enum RecvShape {
    Owned,
    SharedRef,
    MutRef,
}

// Walk an impl's target pattern and collect every `Param` name that
// appears outside any `Ref` / `RawPtr` wrapper. Those are the params
// the implicit `T: Sized` bound bites on — the impl's type *must* have
// a known compile-time size at those positions. Params nested only
// inside `&T` / `*const T` aren't subject to the check (mirror of
// Rust's auto-derived `?Sized` allowance for ref/ptr-only positions).
fn collect_sized_required_params(t: &RType, sized_ctx: bool, out: &mut Vec<String>) {
    match t {
        RType::Param(name) => {
            if sized_ctx && !out.contains(name) {
                out.push(name.clone());
            }
        }
        RType::Struct { type_args, .. } | RType::Enum { type_args, .. } => {
            for arg in type_args {
                collect_sized_required_params(arg, true, out);
            }
        }
        RType::Tuple(elems) => {
            for e in elems {
                collect_sized_required_params(e, true, out);
            }
        }
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => {
            collect_sized_required_params(inner, false, out);
        }
        RType::Slice(inner) => {
            // [T] requires T: Sized (Rust's slice element type).
            collect_sized_required_params(inner, true, out);
        }
        RType::Int(_) | RType::Bool | RType::Str | RType::Never | RType::Char => {}
        RType::AssocProj { .. } => {
            // Conservative: an unconcretized projection isn't itself a
            // bare Param binding, so we don't collect anything from it
            // (the projection's own base param has already been visited
            // in its enclosing context if relevant).
        }
        // Opaque carries no Param bindings — Sized obligations on the
        // hidden concrete type are enforced when the body's actual
        // return type is validated against the slot's bounds.
        RType::Opaque { .. } => {}
        // FnPtr is itself Sized; recurse into params + ret to catch any
        // Param bindings nested under fn-ptr type-args.
        RType::FnPtr { params, ret } => {
            for p in params {
                collect_sized_required_params(p, true, out);
            }
            collect_sized_required_params(ret, true, out);
        }
        // `dyn Trait` is itself unsized but carries no Param-bearing
        // slots — bound paths are canonicalized strings.
        RType::Dyn { .. } => {}
    }
}

// `recv.method(args)` where `recv: &dyn Trait` / `&mut dyn Trait`.
// Phase 2 v1 supports single-bound dyn types only; the method must be
// declared directly on the named trait (supertrait method dispatch is
// gap-tested for now).
fn check_dyn_method_call(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
    bounds: Vec<crate::typeck::InferDynBound>,
    recv_mut: bool,
) -> Result<InferType, Error> {
    if bounds.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: "internal: dyn-trait receiver had empty bound list".to_string(),
            span: mc.method_span.copy(),
        });
    }
    // Canonicalize each bound's trait path and verify obj-safety. Then
    // search each bound's methods for `mc.method`, computing the
    // absolute slot offset within the vtable (slots are laid out
    // post-drop-header in bound order: bound[0]'s methods, then
    // bound[1]'s, etc.). Multiple matches → ambiguity error
    // (disambiguation via UFCS isn't supported yet).
    let canon_paths: Vec<Vec<String>> = bounds
        .iter()
        .map(|b| crate::typeck::canonicalize_dyn_trait_path(
            &b.trait_path,
            ctx.current_module,
            ctx.traits,
            &ctx.use_scope,
            ctx.reexports,
            ctx.current_file,
        ))
        .collect();
    let mut b = 0;
    while b < canon_paths.len() {
        super::dyn_safety::check_object_safety(
            &canon_paths[b],
            ctx.traits,
            &mc.method_span,
            ctx.current_file,
        )?;
        b += 1;
    }
    // Each bound contributes a contiguous run of vtable slots in
    // declaration order — its own methods first, then supertrait
    // methods (BFS via `dyn_vtable_methods`). The absolute slot index
    // is `slot_base + offset_within_bound`.
    let mut found: Vec<(usize, Vec<String>, usize, usize)> = Vec::new(); // (bound_idx, declaring_trait_path, method_idx_in_trait, absolute_slot)
    let mut slot_base: usize = 0;
    let mut b = 0;
    while b < canon_paths.len() {
        // Substitute the dyn type's trait_args (InferType) through to
        // RType for the vtable walker. For supertrait propagation the
        // walker substitutes through SupertraitRef.args.
        let trait_args_rt: Vec<RType> = bounds[b]
            .trait_args
            .iter()
            .map(|t| ctx.subst.finalize(t))
            .collect();
        let methods = super::dyn_safety::dyn_vtable_methods(&canon_paths[b], &trait_args_rt, ctx.traits);
        let mut k = 0;
        while k < methods.len() {
            let (decl_trait, method_idx, _decl_trait_args) = &methods[k];
            let entry = match trait_lookup(ctx.traits, decl_trait) {
                Some(e) => e,
                None => {
                    k += 1;
                    continue;
                }
            };
            if entry.methods[*method_idx].name == mc.method {
                found.push((b, decl_trait.clone(), *method_idx, slot_base + k));
            }
            k += 1;
        }
        slot_base += methods.len();
        b += 1;
    }
    if found.is_empty() {
        let trait_str: Vec<String> = canon_paths.iter().map(|p| place_to_string(p)).collect();
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "no method `{}` on `dyn {}`",
                mc.method,
                trait_str.join(" + "),
            ),
            span: mc.method_span.copy(),
        });
    }
    if found.len() > 1 {
        let trait_str: Vec<String> = found
            .iter()
            .map(|(_, dp, _, _)| place_to_string(dp))
            .collect();
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "ambiguous method `{}` on multi-bound `dyn` type: declared by both `{}` (disambiguation via UFCS isn't supported)",
                mc.method,
                trait_str.join("` and `"),
            ),
            span: mc.method_span.copy(),
        });
    }
    let (bound_idx, declaring_trait_path, method_idx_in_trait, absolute_slot) = found[0].clone();
    let _ = bound_idx;
    let trait_path = declaring_trait_path.clone();
    let entry = trait_lookup(ctx.traits, &trait_path).expect("checked above");
    let method = &entry.methods[method_idx_in_trait];
    // Receiver shape must match the dyn-ref's mutability.
    match (&method.receiver_shape, recv_mut) {
        (Some(TraitReceiverShape::BorrowImm), false)
        | (Some(TraitReceiverShape::BorrowImm), true) // `&self` reachable through `&mut`
        | (Some(TraitReceiverShape::BorrowMut), true) => {}
        (Some(TraitReceiverShape::BorrowMut), false) => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot call `&mut self` method `{}` on `&dyn {}` (need `&mut dyn ...`)",
                    mc.method,
                    place_to_string(&trait_path),
                ),
                span: mc.method_span.copy(),
            });
        }
        _ => return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "method `{}` on `dyn {}` cannot dispatch (no `&self`/`&mut self` receiver)",
                mc.method,
                place_to_string(&trait_path),
            ),
            span: mc.method_span.copy(),
        }),
    }
    // Type-check args against the method's parameter types (skipping
    // the receiver at index 0). Object-safety guarantees no Self in
    // arg or return positions, so no substitution needed.
    let arg_types = &method.param_types[1..];
    if mc.args.len() != arg_types.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}::{}`: expected {}, got {}",
                place_to_string(&trait_path),
                mc.method,
                arg_types.len(),
                mc.args.len(),
            ),
            span: mc.method_span.copy(),
        });
    }
    let mut a = 0;
    while a < mc.args.len() {
        let arg_ty = check_expr(ctx, &mc.args[a])?;
        let expected = rtype_to_infer(&arg_types[a]);
        ctx.subst.coerce(
            &arg_ty,
            &expected,
            ctx.traits,
            ctx.type_params,
            ctx.type_param_bounds,
            &mc.args[a].span,
            ctx.current_file,
        )?;
        a += 1;
    }
    let ret_rt = method
        .return_type
        .clone()
        .unwrap_or(RType::Tuple(Vec::new()));
    // Record the dispatch. `method_idx` is the absolute slot index
    // within the vtable (post-drop-header), accounting for all bounds'
    // method counts that precede the matched bound.
    let id = call_expr.id as usize;
    if id < ctx.dyn_method_calls.len() {
        ctx.dyn_method_calls[id] = Some(crate::typeck::DynMethodDispatch {
            trait_path: trait_path.clone(),
            method_idx: absolute_slot as u32,
            method_param_types: method.param_types[1..].to_vec(),
            method_return_type: ret_rt.clone(),
            recv_mut,
        });
    }
    let _ = method_idx_in_trait; // bound-relative idx, kept for diagnostic context above
    Ok(rtype_to_infer(&ret_rt))
}
