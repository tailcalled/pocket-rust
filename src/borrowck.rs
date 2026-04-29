use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, MethodCall,
    Module, Stmt, StructLit,
};
use crate::span::{Error, Span};
use crate::typeck::{
    CallResolution, FuncTable, MethodResolution, RType, ReceiverAdjust, StructTable, clone_path,
    find_lifetime_source, func_lookup, is_copy, rtype_clone, template_lookup,
};

pub fn check(
    root: &Module,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut current_file = root.source_file.clone();
    let mut current_module: Vec<String> = Vec::new();
    push_root_name(&mut current_module, root);
    check_module(root, &mut current_module, &mut current_file, structs, funcs)?;
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn check_module(
    module: &Module,
    current_module: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => check_function(
                f,
                current_module,
                current_module,
                None,
                current_file,
                structs,
                funcs,
            )?,
            Item::Module(m) => {
                current_module.push(m.name.clone());
                check_module(m, current_module, current_file, structs, funcs)?;
                current_module.pop();
            }
            Item::Struct(_) => {}
            Item::Impl(ib) => {
                if ib.target.segments.len() != 1 {
                    continue;
                }
                let target_name = ib.target.segments[0].name.clone();
                let mut method_prefix = clone_path(current_module);
                method_prefix.push(target_name.clone());
                let mut target_full = clone_path(current_module);
                target_full.push(target_name);
                let mut impl_param_args: Vec<RType> = Vec::new();
                let mut k = 0;
                while k < ib.type_params.len() {
                    impl_param_args.push(RType::Param(ib.type_params[k].name.clone()));
                    k += 1;
                }
                let mut impl_lifetime_args: Vec<crate::typeck::LifetimeRepr> = Vec::new();
                let mut k = 0;
                while k < ib.lifetime_params.len() {
                    impl_lifetime_args.push(crate::typeck::LifetimeRepr::Named(
                        ib.lifetime_params[k].name.clone(),
                    ));
                    k += 1;
                }
                let target_rt = RType::Struct {
                    path: target_full,
                    type_args: impl_param_args,
                    lifetime_args: impl_lifetime_args,
                };
                let mut k = 0;
                while k < ib.methods.len() {
                    check_function(
                        &ib.methods[k],
                        current_module,
                        &method_prefix,
                        Some(rtype_clone(&target_rt)),
                        current_file,
                        structs,
                        funcs,
                    )?;
                    k += 1;
                }
            }
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
    self_target: Option<RType>,
    current_file: &str,
    _structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut full = clone_path(path_prefix);
    full.push(func.name.clone());
    // The function may be a regular entry or a generic template — peel both.
    let (param_types, expr_types, method_resolutions, call_resolutions) =
        if let Some(entry) = func_lookup(funcs, &full) {
            (
                &entry.param_types,
                &entry.expr_types,
                &entry.method_resolutions,
                &entry.call_resolutions,
            )
        } else if let Some((_, t)) = template_lookup(funcs, &full) {
            (
                &t.param_types,
                &t.expr_types,
                &t.method_resolutions,
                &t.call_resolutions,
            )
        } else {
            unreachable!("typeck registered this function");
        };

    let liveness = compute_liveness(&func.body, &func.params);
    let mut state = BorrowState {
        holders: Vec::new(),
        moved: Vec::new(),
        expr_types,
        method_resolutions,
        call_resolutions,
        file: current_file.to_string(),
        funcs,
        current_module,
        self_target,
        liveness,
        current_step: 0,
    };

    let mut k = 0;
    while k < func.params.len() {
        state.holders.push(Holder {
            name: Some(func.params[k].name.clone()),
            rtype: Some(rtype_clone(&param_types[k])),
            holds: Vec::new(),
            field_holds: Vec::new(),
        });
        k += 1;
    }

    walk_stmts_and_tail(&mut state, &func.body)?;
    Ok(())
}

// ---------- State ----------

struct BorrowState<'a> {
    // Stack of holders. A holder either names a let/param binding (Some name)
    // or is a synthetic call slot (None name). Each holder records the
    // borrows it currently keeps alive (a list of place paths).
    holders: Vec<Holder>,
    // Permanent set of moved places (function-wide).
    moved: Vec<Vec<String>>,
    // Per-NodeId resolved types/resolutions populated by typeck. Borrowck
    // looks up by `expr.id` rather than maintaining a source-DFS counter.
    expr_types: &'a Vec<Option<RType>>,
    method_resolutions: &'a Vec<Option<MethodResolution>>,
    call_resolutions: &'a Vec<Option<CallResolution>>,
    file: String,
    funcs: &'a FuncTable,
    #[allow(dead_code)]
    current_module: &'a Vec<String>,
    #[allow(dead_code)]
    self_target: Option<RType>,
    // Liveness — name → last-use step, computed by a pre-pass over the body.
    // Holders whose binding's last_use < current_step have their borrows
    // garbage-collected (cleared) after each step.
    liveness: Liveness,
    current_step: u32,
}

struct Liveness {
    last_use: Vec<(String, u32)>,
}

struct Holder {
    name: Option<String>,
    rtype: Option<RType>,
    holds: Vec<HeldBorrow>,
    // Per-slot borrows for struct-typed bindings whose fields hold refs.
    // Each entry tags a field path with the borrows tied to that slot. A
    // read of `binding.field` (where field is ref-typed) returns the
    // matching entry's borrows; moving the binding transfers them to the
    // new holder.
    field_holds: Vec<FieldHold>,
}

struct HeldBorrow {
    place: Vec<String>,
    mutable: bool,
}

// One per-slot record: a field path within a struct holder, plus the
// borrows tied to that slot. Phase D's minimal scheme uses single-segment
// `field` paths (top-level fields only); nested struct-with-ref fields
// aren't tracked at the holder level.
struct FieldHold {
    field: String,
    borrows: Vec<HeldBorrow>,
}

// A descriptor of the borrows a value carries forward — i.e. which places this
// expression's value, if it's a reference, refers to. For non-reference values,
// `borrows` is empty; if the value is a struct with ref fields,
// `field_borrows` records the per-slot borrows so a binding holder can
// preserve them under the per-slot tracking model. The caller decides what
// to do with these (absorb into a binding, attach to a call slot, drop).
struct ValueDesc {
    borrows: Vec<HeldBorrow>,
    field_borrows: Vec<FieldHold>,
}

fn empty_desc() -> ValueDesc {
    ValueDesc {
        borrows: Vec::new(),
        field_borrows: Vec::new(),
    }
}

fn clone_field_holds(v: &Vec<FieldHold>) -> Vec<FieldHold> {
    let mut out: Vec<FieldHold> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(FieldHold {
            field: v[i].field.clone(),
            borrows: clone_held_borrows(&v[i].borrows),
        });
        i += 1;
    }
    out
}

// ---------- Walk ----------

fn walk_stmts_and_tail(state: &mut BorrowState, block: &Block) -> Result<ValueDesc, Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => walk_let_stmt(state, let_stmt)?,
            Stmt::Assign(assign) => walk_assign_stmt(state, assign)?,
            Stmt::Expr(expr) => {
                walk_expr(state, expr)?;
            }
        }
        state.current_step += 1;
        gc_dead_holders(state);
        i += 1;
    }
    match &block.tail {
        Some(tail) => {
            let desc = walk_expr(state, tail)?;
            state.current_step += 1;
            gc_dead_holders(state);
            Ok(desc)
        }
        None => Ok(empty_desc()),
    }
}

// After each step, holders whose binding's last-use step is strictly less than
// `current_step` are no longer live — their borrows are dropped. Implements
// straight-line NLL: a borrow lives until the binding's last use, not until
// scope end.
fn gc_dead_holders(state: &mut BorrowState) {
    let mut i = 0;
    while i < state.holders.len() {
        if let Some(name) = &state.holders[i].name {
            let lu = liveness_lookup(&state.liveness, name);
            match lu {
                Some(s) if s >= state.current_step => {}
                _ => state.holders[i].holds.clear(),
            }
        }
        i += 1;
    }
}

fn liveness_lookup(info: &Liveness, name: &str) -> Option<u32> {
    let mut i = 0;
    while i < info.last_use.len() {
        if info.last_use[i].0 == name {
            return Some(info.last_use[i].1);
        }
        i += 1;
    }
    None
}

fn liveness_record(info: &mut Liveness, name: &str, step: u32) {
    let mut i = 0;
    while i < info.last_use.len() {
        if info.last_use[i].0 == name {
            if info.last_use[i].1 < step {
                info.last_use[i].1 = step;
            }
            return;
        }
        i += 1;
    }
    info.last_use.push((name.to_string(), step));
}

fn compute_liveness(body: &Block, params: &Vec<crate::ast::Param>) -> Liveness {
    let mut info = Liveness {
        last_use: Vec::new(),
    };
    // Seed each parameter at step 0 — their borrows are held by holders from
    // the start; the GC pass should keep them around until they're actually
    // referenced (or longer if referenced later).
    let mut i = 0;
    while i < params.len() {
        liveness_record(&mut info, &params[i].name, 0);
        i += 1;
    }
    let mut step: u32 = 0;
    walk_block_for_liveness(body, &mut step, &mut info);
    info
}

fn walk_block_for_liveness(block: &Block, step: &mut u32, info: &mut Liveness) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                walk_expr_for_liveness(&let_stmt.value, step, info);
                // Anchor the new binding's lifetime at the let-stmt's step;
                // later reads bump it. Without this, an unused binding would
                // never appear in `last_use` and `liveness_lookup` would return
                // None — which the GC treats as "dead immediately." That's
                // the desired behavior, but recording it explicitly keeps the
                // semantics readable.
                liveness_record(info, &let_stmt.name, *step);
            }
            Stmt::Assign(assign) => {
                walk_expr_for_liveness(&assign.lhs, step, info);
                walk_expr_for_liveness(&assign.rhs, step, info);
            }
            Stmt::Expr(expr) => walk_expr_for_liveness(expr, step, info),
        }
        *step += 1;
        i += 1;
    }
    if let Some(tail) = &block.tail {
        walk_expr_for_liveness(tail, step, info);
        *step += 1;
    }
}

fn walk_expr_for_liveness(expr: &Expr, step: &mut u32, info: &mut Liveness) {
    match &expr.kind {
        ExprKind::IntLit(_) => {}
        ExprKind::Var(name) => liveness_record(info, name, *step),
        ExprKind::Borrow { inner, .. } => walk_expr_for_liveness(inner, step, info),
        ExprKind::FieldAccess(fa) => walk_expr_for_liveness(&fa.base, step, info),
        ExprKind::Cast { inner, .. } => walk_expr_for_liveness(inner, step, info),
        ExprKind::Deref(inner) => walk_expr_for_liveness(inner, step, info),
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                walk_expr_for_liveness(&c.args[i], step, info);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                walk_expr_for_liveness(&s.fields[i].value, step, info);
                i += 1;
            }
        }
        ExprKind::MethodCall(mc) => {
            walk_expr_for_liveness(&mc.receiver, step, info);
            let mut i = 0;
            while i < mc.args.len() {
                walk_expr_for_liveness(&mc.args[i], step, info);
                i += 1;
            }
        }
        ExprKind::Block(b) | ExprKind::Unsafe(b) => {
            // Inner block stmts share the same step counter as the outer walk —
            // borrowck's actual walk also advances `current_step` inside inner
            // blocks (via walk_stmts_and_tail), so the two passes stay in sync.
            walk_block_for_liveness(b.as_ref(), step, info);
        }
    }
}

fn walk_assign_stmt(state: &mut BorrowState, assign: &AssignStmt) -> Result<(), Error> {
    // Deref-rooted writes (`*p = …;`, `(*p).f = …;`): writing through a
    // ref/raw-ptr exclusively (`&mut`/`*mut`) is authorized by typeck. Borrow
    // tracking can't precisely identify the underlying place (we'd need alias
    // analysis), so we just walk the inner deref target and the RHS for their
    // side effects and skip the conflict scan.
    if is_deref_rooted_assign(&assign.lhs) {
        walk_assign_lhs(state, &assign.lhs)?;
        walk_expr(state, &assign.rhs)?;
        return Ok(());
    }
    let chain = extract_place(&assign.lhs)
        .expect("typeck verified the assignment LHS is a place expression");
    // Reject if any holder has an overlapping path — assignment can't proceed
    // while the target memory is borrowed.
    // Skip the conflict scan when the assignment is *through* a `&mut` binding —
    // the borrow on that binding is the very thing that authorizes the write.
    let through_mut_ref = if chain.len() > 1 {
        let mut found: Option<usize> = None;
        let mut i = state.holders.len();
        while i > 0 {
            i -= 1;
            if let Some(n) = &state.holders[i].name {
                if n == &chain[0] {
                    found = Some(i);
                    break;
                }
            }
        }
        match found {
            Some(idx) => matches!(
                state.holders[idx].rtype,
                Some(RType::Ref { mutable: true, .. })
            ),
            None => false,
        }
    } else {
        false
    };
    if !through_mut_ref {
        let mut h = 0;
        while h < state.holders.len() {
            let mut k = 0;
            while k < state.holders[h].holds.len() {
                if paths_share_prefix(&state.holders[h].holds[k].place, &chain) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot assign to `{}` while it is borrowed",
                            place_to_string(&chain)
                        ),
                        span: assign.span.copy(),
                    });
                }
                k += 1;
            }
            h += 1;
        }
    }
    // Walk the RHS for its move/borrow effects.
    let desc = walk_expr(state, &assign.rhs)?;
    // RHS desc would carry borrows iff the result is a ref. Assignment to a
    // non-ref binding can't accept ref-typed values (typeck enforced); assignment
    // to a ref binding (e.g. `let mut r: &T; r = …;`) treats the new value the
    // same way the binding's `let` would have. For simplicity, drop the desc
    // here — the binding is already a holder, and reassignment doesn't change
    // which holder owns existing borrows. (This means once-borrowed-always-tied
    // for a ref binding; we can refine later.)
    let _ = desc;
    // The assigned place is now fresh; clear any moves recorded on it or below.
    let mut new_moved: Vec<Vec<String>> = Vec::new();
    let mut i = 0;
    while i < state.moved.len() {
        if !chain_is_prefix_of(&chain, &state.moved[i]) {
            let mut copy: Vec<String> = Vec::new();
            let mut k = 0;
            while k < state.moved[i].len() {
                copy.push(state.moved[i][k].clone());
                k += 1;
            }
            new_moved.push(copy);
        }
        i += 1;
    }
    state.moved = new_moved;
    Ok(())
}

fn chain_is_prefix_of(prefix: &Vec<String>, full: &Vec<String>) -> bool {
    if prefix.len() > full.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if prefix[i] != full[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn walk_let_stmt(state: &mut BorrowState, let_stmt: &LetStmt) -> Result<(), Error> {
    let desc = walk_expr(state, &let_stmt.value)?;
    let ty = rtype_clone(
        state.expr_types[let_stmt.value.id as usize]
            .as_ref()
            .expect("typeck recorded this binding's type"),
    );
    state.holders.push(Holder {
        name: Some(let_stmt.name.clone()),
        rtype: Some(ty),
        holds: desc.borrows,
        field_holds: desc.field_borrows,
    });
    Ok(())
}

fn walk_expr(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    match &expr.kind {
        ExprKind::IntLit(_) => Ok(empty_desc()),
        ExprKind::Var(name) => walk_var(state, name, expr),
        ExprKind::Call(call) => walk_call(state, call, expr.id),
        ExprKind::StructLit(lit) => walk_struct_lit(state, lit),
        ExprKind::FieldAccess(fa) => walk_field_access(state, fa, expr),
        ExprKind::Borrow { .. } => walk_borrow(state, expr),
        ExprKind::Cast { inner, .. } => {
            // The inner produces side effects (moves, registered borrows) that
            // we still want to surface, but the cast itself yields a raw
            // pointer with no compile-time lifetime tracking — drop the
            // borrows so they don't get re-attached downstream.
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
        ExprKind::Deref(inner) => {
            // Deref reads through a ref/raw-ptr and yields the pointed-at
            // value. Refs/raw-ptrs are Copy, so reading them clones the
            // borrow handle — but typeck rejects deref of non-Copy inner, so
            // the resulting value carries no borrows of its own.
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
        ExprKind::Unsafe(block) => walk_block_expr(state, block.as_ref()),
        ExprKind::Block(block) => walk_block_expr(state, block.as_ref()),
        ExprKind::MethodCall(mc) => walk_method_call(state, mc, expr.id),
    }
}

fn walk_method_call(
    state: &mut BorrowState,
    mc: &MethodCall,
    node_id: crate::ast::NodeId,
) -> Result<ValueDesc, Error> {
    let res = state.method_resolutions[node_id as usize]
        .as_ref()
        .expect("typeck registered this method call");
    let recv_adjust = match &res.recv_adjust {
        ReceiverAdjust::Move => RecvAdjustLocal::Move,
        ReceiverAdjust::BorrowImm => RecvAdjustLocal::BorrowImm,
        ReceiverAdjust::BorrowMut => RecvAdjustLocal::BorrowMut,
        ReceiverAdjust::ByRef => RecvAdjustLocal::ByRef,
    };
    let ret_borrows_recv = res.ret_borrows_receiver;
    // Push synthetic call slot.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let call_idx = state.holders.len() - 1;
    // Process the receiver per recv_adjust.
    let recv_borrows: Vec<HeldBorrow> = match recv_adjust {
        RecvAdjustLocal::Move => {
            // Treat recv as an arg — walk it for moves, absorb borrows.
            let desc = walk_expr(state, &mc.receiver)?;
            let snapshot = clone_held_borrows(&desc.borrows);
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: clone_path(&desc.borrows[k].place),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &mc.receiver.span)?;
                state.holders[call_idx].holds.push(new);
                k += 1;
            }
            snapshot
        }
        RecvAdjustLocal::BorrowImm | RecvAdjustLocal::BorrowMut => {
            // Synthesize a borrow on recv (recv must be a place expr; typeck verified).
            let mutable = matches!(recv_adjust, RecvAdjustLocal::BorrowMut);
            walk_synth_borrow(state, &mc.receiver, mutable, call_idx)?
        }
        RecvAdjustLocal::ByRef => {
            // Recv is already a ref — walk as a regular var read; its borrows
            // get absorbed into the call slot (and snapshotted for propagation).
            let desc = walk_expr(state, &mc.receiver)?;
            let snapshot = clone_held_borrows(&desc.borrows);
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: clone_path(&desc.borrows[k].place),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &mc.receiver.span)?;
                state.holders[call_idx].holds.push(new);
                k += 1;
            }
            snapshot
        }
    };
    // Process remaining args. Per-slot field_borrows from struct args are
    // flattened into the call slot alongside direct borrows.
    let mut i = 0;
    while i < mc.args.len() {
        let desc = walk_expr(state, &mc.args[i])?;
        let mut k = 0;
        while k < desc.borrows.len() {
            let new = HeldBorrow {
                place: clone_path(&desc.borrows[k].place),
                mutable: desc.borrows[k].mutable,
            };
            check_borrow_conflict(state, &new, &mc.args[i].span)?;
            state.holders[call_idx].holds.push(new);
            k += 1;
        }
        let mut f = 0;
        while f < desc.field_borrows.len() {
            let mut k = 0;
            while k < desc.field_borrows[f].borrows.len() {
                let new = HeldBorrow {
                    place: clone_path(&desc.field_borrows[f].borrows[k].place),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &mc.args[i].span)?;
                state.holders[call_idx].holds.push(new);
                k += 1;
            }
            f += 1;
        }
        i += 1;
    }
    state.holders.truncate(call_idx);
    if ret_borrows_recv {
        Ok(ValueDesc {
            borrows: recv_borrows,
            field_borrows: Vec::new(),
        })
    } else {
        Ok(empty_desc())
    }
}

enum RecvAdjustLocal {
    Move,
    BorrowImm,
    BorrowMut,
    ByRef,
}

// Synthesize a `&recv` (or `&mut recv`) borrow, with the same conflict checks
// `walk_borrow` would apply, and absorb the result into the call slot.
fn walk_synth_borrow(
    state: &mut BorrowState,
    inner: &Expr,
    mutable: bool,
    call_idx: usize,
) -> Result<Vec<HeldBorrow>, Error> {
    let place = match extract_place(inner) {
        Some(p) => p,
        None => {
            // Non-place receiver — autoref of a temporary. Walk for side
            // effects; produces no borrow.
            walk_expr(state, inner)?;
            return Ok(Vec::new());
        }
    };
    // Check it hasn't been moved.
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i], &place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!(
                    "cannot borrow `{}`: it has been moved",
                    place_to_string(&place)
                ),
                span: inner.span.copy(),
            });
        }
        i += 1;
    }
    let new = HeldBorrow {
        place: clone_path(&place),
        mutable,
    };
    check_borrow_conflict(state, &new, &inner.span)?;
    state.holders[call_idx].holds.push(new);
    let mut snapshot: Vec<HeldBorrow> = Vec::new();
    snapshot.push(HeldBorrow { place, mutable });
    Ok(snapshot)
}

fn walk_var(state: &mut BorrowState, name: &str, expr: &Expr) -> Result<ValueDesc, Error> {
    let idx = find_binding(state, name).expect("typeck verified the variable exists");
    if is_raw_ptr_holder(&state.holders[idx]) {
        // Raw pointers are Copy and carry no borrow handles.
        return Ok(empty_desc());
    }
    if is_ref_holder(&state.holders[idx]) {
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        check_not_moved(state, &place, &expr.span)?;
        if is_mut_ref_holder(&state.holders[idx]) {
            // `&mut T` is not really Copy under our borrow model — we don't
            // implement implicit reborrow, so reading a `&mut` binding moves
            // its borrow into the consumer (call slot or new binding) and the
            // binding becomes unusable afterward. Liveness GC alone isn't
            // sufficient because both the source binding and the consumer
            // would otherwise hold the same exclusive borrow during arg
            // evaluation.
            let mut taken: Vec<HeldBorrow> = Vec::new();
            std::mem::swap(&mut taken, &mut state.holders[idx].holds);
            state.moved.push(place);
            Ok(ValueDesc { borrows: taken, field_borrows: Vec::new() })
        } else {
            // `&T` is Copy: cloning the borrow handle is fine.
            let holds = clone_held_borrows(&state.holders[idx].holds);
            Ok(ValueDesc { borrows: holds, field_borrows: Vec::new() })
        }
    } else if is_owned_copy_holder(&state.holders[idx]) {
        // Owned Copy primitive (ints, etc.): reading is a value copy, no move,
        // no borrows to forward. Still must refuse reads from a moved place.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        check_not_moved(state, &place, &expr.span)?;
        Ok(empty_desc())
    } else {
        // Owned non-Copy (struct): tracked as a move. If the holder has
        // per-slot field_holds (Phase D: struct with ref fields), transfer
        // them into the consumer's desc so the new binding/call slot keeps
        // those borrows alive.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        try_move(state, place, expr.span.copy())?;
        let mut taken: Vec<FieldHold> = Vec::new();
        std::mem::swap(&mut taken, &mut state.holders[idx].field_holds);
        Ok(ValueDesc {
            borrows: Vec::new(),
            field_borrows: taken,
        })
    }
}

fn walk_call(
    state: &mut BorrowState,
    call: &Call,
    node_id: crate::ast::NodeId,
) -> Result<ValueDesc, Error> {
    // Phase D: borrow propagation through ref-returning calls flows along
    // lifetimes. Look up the callee's `ret_lifetime`; collect every param
    // whose outermost lifetime matches — those args' borrows all propagate
    // into the result (combined borrow sets when one lifetime ties to
    // multiple args).
    let ret_ref_sources: Vec<usize> = match state.call_resolutions[node_id as usize]
        .as_ref()
        .expect("typeck registered this call")
    {
        CallResolution::Direct(idx) => {
            let entry = &state.funcs.entries[*idx];
            match &entry.ret_lifetime {
                Some(rl) => find_lifetime_source(&entry.param_lifetimes, rl),
                None => Vec::new(),
            }
        }
        CallResolution::Generic { template_idx, .. } => {
            let t = &state.funcs.templates[*template_idx];
            match &t.ret_lifetime {
                Some(rl) => find_lifetime_source(&t.param_lifetimes, rl),
                None => Vec::new(),
            }
        }
    };

    // Push a synthetic call holder. Borrows produced by argument expressions
    // become its holds for the duration of the call, then the holder is popped.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let call_idx = state.holders.len() - 1;
    // Snapshot each arg's borrows (including any per-slot field borrows
    // flattened together) before they're absorbed into the call slot, so
    // we can later attach the source arg's borrows to the result desc.
    let mut arg_borrow_snapshots: Vec<Vec<HeldBorrow>> = Vec::new();
    let mut i = 0;
    while i < call.args.len() {
        let desc = walk_expr(state, &call.args[i])?;
        // Combine direct + per-slot borrows into one flat snapshot.
        let mut combined: Vec<HeldBorrow> = clone_held_borrows(&desc.borrows);
        let mut f = 0;
        while f < desc.field_borrows.len() {
            let mut k = 0;
            while k < desc.field_borrows[f].borrows.len() {
                combined.push(HeldBorrow {
                    place: clone_path(&desc.field_borrows[f].borrows[k].place),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                });
                k += 1;
            }
            f += 1;
        }
        arg_borrow_snapshots.push(clone_held_borrows(&combined));
        let mut k = 0;
        while k < combined.len() {
            // Conflict-check the new borrow against every other holder's holds.
            let new = HeldBorrow {
                place: clone_path(&combined[k].place),
                mutable: combined[k].mutable,
            };
            check_borrow_conflict(state, &new, &call.args[i].span)?;
            state.holders[call_idx].holds.push(new);
            k += 1;
        }
        i += 1;
    }
    state.holders.truncate(call_idx);
    if ret_ref_sources.is_empty() {
        return Ok(empty_desc());
    }
    // Combine borrow sets from every matching arg slot.
    let mut combined: Vec<HeldBorrow> = Vec::new();
    let mut s = 0;
    while s < ret_ref_sources.len() {
        let idx = ret_ref_sources[s];
        let mut k = 0;
        while k < arg_borrow_snapshots[idx].len() {
            combined.push(HeldBorrow {
                place: clone_path(&arg_borrow_snapshots[idx][k].place),
                mutable: arg_borrow_snapshots[idx][k].mutable,
            });
            k += 1;
        }
        s += 1;
    }
    Ok(ValueDesc {
        borrows: combined,
        field_borrows: Vec::new(),
    })
}

fn check_borrow_conflict(
    state: &BorrowState,
    new: &HeldBorrow,
    span: &Span,
) -> Result<(), Error> {
    let mut h = 0;
    while h < state.holders.len() {
        let mut k = 0;
        while k < state.holders[h].holds.len() {
            let other = &state.holders[h].holds[k];
            if paths_share_prefix(&other.place, &new.place)
                && (other.mutable || new.mutable)
            {
                let kind = if new.mutable { "mutable" } else { "shared" };
                let other_kind = if other.mutable { "mutable" } else { "shared" };
                return Err(Error {
                    file: state.file.clone(),
                    message: format!(
                        "cannot borrow `{}` as {}: already borrowed as {}",
                        place_to_string(&new.place),
                        kind,
                        other_kind
                    ),
                    span: span.copy(),
                });
            }
            k += 1;
        }
        // Phase D: per-slot field_holds also count as live borrows.
        let mut f = 0;
        while f < state.holders[h].field_holds.len() {
            let mut k = 0;
            while k < state.holders[h].field_holds[f].borrows.len() {
                let other = &state.holders[h].field_holds[f].borrows[k];
                if paths_share_prefix(&other.place, &new.place)
                    && (other.mutable || new.mutable)
                {
                    let kind = if new.mutable { "mutable" } else { "shared" };
                    let other_kind = if other.mutable { "mutable" } else { "shared" };
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot borrow `{}` as {}: already borrowed as {}",
                            place_to_string(&new.place),
                            kind,
                            other_kind
                        ),
                        span: span.copy(),
                    });
                }
                k += 1;
            }
            f += 1;
        }
        h += 1;
    }
    Ok(())
}

fn walk_struct_lit(state: &mut BorrowState, lit: &StructLit) -> Result<ValueDesc, Error> {
    // Phase D: a struct field may be a ref. Each field initializer's borrows
    // get tagged with the field name and propagated as `field_borrows` of
    // the resulting value, so a binding holder can keep per-slot tracking.
    // While walking field initializers we push a synthetic holder so any
    // in-flight borrows from earlier fields are visible to conflict checks
    // in later fields' initializers.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let synth_idx = state.holders.len() - 1;
    let mut field_borrows: Vec<FieldHold> = Vec::new();
    let mut i = 0;
    while i < lit.fields.len() {
        let desc = walk_expr(state, &lit.fields[i].value)?;
        if !desc.borrows.is_empty() {
            // Tag this slot's borrows. Also register them in the synthetic
            // holder so subsequent fields' borrows see the conflict.
            let mut grouped: Vec<HeldBorrow> = Vec::new();
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: clone_path(&desc.borrows[k].place),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &lit.fields[i].value.span)?;
                state.holders[synth_idx].holds.push(new);
                grouped.push(HeldBorrow {
                    place: clone_path(&desc.borrows[k].place),
                    mutable: desc.borrows[k].mutable,
                });
                k += 1;
            }
            field_borrows.push(FieldHold {
                field: lit.fields[i].name.clone(),
                borrows: grouped,
            });
        }
        // Phase D minimal: nested per-slot (struct-with-ref inside another
        // struct-with-ref) isn't tracked; field initializer's own
        // `field_borrows` are dropped here.
        i += 1;
    }
    state.holders.truncate(synth_idx);
    Ok(ValueDesc {
        borrows: Vec::new(),
        field_borrows,
    })
}

fn walk_field_access(
    state: &mut BorrowState,
    fa: &FieldAccess,
    expr: &Expr,
) -> Result<ValueDesc, Error> {
    match extract_place(expr) {
        Some(place) => {
            let root_idx =
                find_binding(state, &place[0]).expect("typeck verified the variable exists");
            if is_ref_holder(&state.holders[root_idx]) {
                // Navigation through a reference. Field is Copy (typeck), so
                // the result is plain data with no carried borrows; the ref
                // itself is unchanged.
                Ok(empty_desc())
            } else {
                // Field access on an owned root.
                let field_ty = state.expr_types[expr.id as usize]
                    .as_ref()
                    .map(rtype_clone);
                let field_is_ref = matches!(&field_ty, Some(RType::Ref { .. }));
                let field_is_copy = field_ty.as_ref().map(is_copy).unwrap_or(false);
                check_not_moved(state, &place, &expr.span)?;
                // Phase D: if the field is ref-typed and a top-level field
                // (path length == 2: root + field), look up the holder's
                // per-slot field_holds and propagate those borrows.
                if field_is_ref && place.len() == 2 {
                    let mut found_borrows: Vec<HeldBorrow> = Vec::new();
                    let mut k = 0;
                    while k < state.holders[root_idx].field_holds.len() {
                        if state.holders[root_idx].field_holds[k].field == place[1] {
                            found_borrows = clone_held_borrows(
                                &state.holders[root_idx].field_holds[k].borrows,
                            );
                            break;
                        }
                        k += 1;
                    }
                    return Ok(ValueDesc {
                        borrows: found_borrows,
                        field_borrows: Vec::new(),
                    });
                }
                if field_is_copy {
                    // already checked not-moved above
                } else {
                    try_move(state, place, expr.span.copy())?;
                }
                Ok(empty_desc())
            }
        }
        None => {
            // Field access on a non-place base (e.g. a call result). Walk the
            // base for its side effects; the field result is Copy.
            walk_expr(state, &fa.base)?;
            Ok(empty_desc())
        }
    }
}

fn walk_borrow(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    let (inner, mutable) = match &expr.kind {
        ExprKind::Borrow { inner, mutable } => (inner.as_ref(), *mutable),
        _ => unreachable!("walk_borrow called on non-Borrow"),
    };
    match extract_place(inner) {
        Some(place) => {
            // Refuse to borrow a place whose root has already been moved.
            let mut i = 0;
            while i < state.moved.len() {
                if paths_share_prefix(&state.moved[i], &place) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot borrow `{}`: it has been moved",
                            place_to_string(&place)
                        ),
                        span: expr.span.copy(),
                    });
                }
                i += 1;
            }
            let new = HeldBorrow {
                place: clone_path(&place),
                mutable,
            };
            check_borrow_conflict(state, &new, &expr.span)?;
            let mut borrows = Vec::new();
            borrows.push(HeldBorrow { place, mutable });
            Ok(ValueDesc { borrows, field_borrows: Vec::new() })
        }
        None => {
            // Borrowing a non-place expression (e.g. `&fresh_struct_lit()`).
            // We still walk inner for its move-tracking side effects, but
            // don't track the borrow (no place to point at).
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
    }
}

fn walk_block_expr(state: &mut BorrowState, block: &Block) -> Result<ValueDesc, Error> {
    // The block introduces a fresh local scope. Any holders pushed inside
    // (let bindings) are dropped when the block ends. The block's tail value
    // descriptor is returned to the caller — its borrows survive the scope
    // because the *caller* will turn them into a holder of its own.
    let mark = state.holders.len();
    let desc = walk_stmts_and_tail(state, block)?;
    state.holders.truncate(mark);
    Ok(desc)
}

// ---------- Helpers ----------

fn find_binding(state: &BorrowState, name: &str) -> Option<usize> {
    let mut i = state.holders.len();
    while i > 0 {
        i -= 1;
        if let Some(n) = &state.holders[i].name {
            if n == name {
                return Some(i);
            }
        }
    }
    None
}

fn is_ref_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::Ref { .. }))
}

fn is_raw_ptr_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::RawPtr { .. }))
}

fn is_mut_ref_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::Ref { mutable: true, .. }))
}

// True for owned Copy primitives (ints currently; not refs or raw pointers,
// which are handled by their own dedicated branches in walk_var). Reading
// such a binding produces a value copy — no move, no borrow to forward.
fn is_owned_copy_holder(h: &Holder) -> bool {
    match &h.rtype {
        Some(RType::Int(_)) => true,
        _ => false,
    }
}


fn is_deref_rooted_assign(expr: &Expr) -> bool {
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Deref(_) => return true,
            ExprKind::FieldAccess(fa) => current = &fa.base,
            _ => return false,
        }
    }
}

// Walk the deref-rooted LHS for its side effects: typically the chain of
// FieldAccess/Deref nodes ends at a Var (the &mut binding being written
// through), and we want to surface that read.
fn walk_assign_lhs(state: &mut BorrowState, expr: &Expr) -> Result<(), Error> {
    match &expr.kind {
        ExprKind::Deref(inner) => {
            walk_expr(state, inner)?;
            Ok(())
        }
        ExprKind::FieldAccess(fa) => walk_assign_lhs(state, &fa.base),
        _ => {
            walk_expr(state, expr)?;
            Ok(())
        }
    }
}

fn extract_place(expr: &Expr) -> Option<Vec<String>> {
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
            _ => return None,
        }
    }
}

// Check that a place hasn't already been moved out of. Used for Copy reads
// (which don't add to the moved set but still must refuse to read from a
// moved place).
fn check_not_moved(
    state: &BorrowState,
    place: &Vec<String>,
    span: &Span,
) -> Result<(), Error> {
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i], place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!("`{}` was already moved", place_to_string(place)),
                span: span.copy(),
            });
        }
        i += 1;
    }
    Ok(())
}

fn try_move(state: &mut BorrowState, place: Vec<String>, span: Span) -> Result<(), Error> {
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i], &place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!("`{}` was already moved", place_to_string(&place)),
                span,
            });
        }
        i += 1;
    }
    let mut h = 0;
    while h < state.holders.len() {
        let mut k = 0;
        while k < state.holders[h].holds.len() {
            if paths_share_prefix(&state.holders[h].holds[k].place, &place) {
                return Err(Error {
                    file: state.file.clone(),
                    message: format!(
                        "cannot move `{}` while it is borrowed",
                        place_to_string(&place)
                    ),
                    span,
                });
            }
            k += 1;
        }
        // Also scan per-slot field_holds (Phase D).
        let mut f = 0;
        while f < state.holders[h].field_holds.len() {
            let mut k = 0;
            while k < state.holders[h].field_holds[f].borrows.len() {
                if paths_share_prefix(
                    &state.holders[h].field_holds[f].borrows[k].place,
                    &place,
                ) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot move `{}` while it is borrowed",
                            place_to_string(&place)
                        ),
                        span,
                    });
                }
                k += 1;
            }
            f += 1;
        }
        h += 1;
    }
    state.moved.push(place);
    Ok(())
}

fn paths_share_prefix(a: &Vec<String>, b: &Vec<String>) -> bool {
    let m = if a.len() < b.len() { a.len() } else { b.len() };
    let mut i = 0;
    while i < m {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn place_to_string(p: &Vec<String>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < p.len() {
        if i > 0 {
            s.push('.');
        }
        s.push_str(&p[i]);
        i += 1;
    }
    s
}

fn clone_held_borrows(holds: &Vec<HeldBorrow>) -> Vec<HeldBorrow> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < holds.len() {
        out.push(HeldBorrow {
            place: clone_path(&holds[i].place),
            mutable: holds[i].mutable,
        });
        i += 1;
    }
    out
}
