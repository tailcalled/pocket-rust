use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, MethodCall,
    Module, Stmt, StructLit,
};
use crate::span::{Error, Span};
use crate::typeck::{
    CallResolution, FuncTable, MethodResolution, RType, ReceiverAdjust, StructTable, clone_path,
    func_lookup, rtype_clone, template_lookup,
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
                let target_rt = RType::Struct {
                    path: target_full,
                    type_args: impl_param_args,
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
    let (param_types, let_types, method_resolutions, call_resolutions) =
        if let Some(entry) = func_lookup(funcs, &full) {
            (
                &entry.param_types,
                &entry.let_types,
                &entry.method_resolutions,
                &entry.call_resolutions,
            )
        } else if let Some((_, t)) = template_lookup(funcs, &full) {
            (
                &t.param_types,
                &t.let_types,
                &t.method_resolutions,
                &t.call_resolutions,
            )
        } else {
            unreachable!("typeck registered this function");
        };

    let mut let_types_owned: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < let_types.len() {
        let_types_owned.push(rtype_clone(&let_types[k]));
        k += 1;
    }

    let mut state = BorrowState {
        holders: Vec::new(),
        moved: Vec::new(),
        let_types: let_types_owned,
        let_idx: 0,
        method_resolutions,
        method_idx: 0,
        call_resolutions,
        call_idx: 0,
        file: current_file.to_string(),
        funcs,
        current_module,
        self_target,
    };

    let mut k = 0;
    while k < func.params.len() {
        state.holders.push(Holder {
            name: Some(func.params[k].name.clone()),
            rtype: Some(rtype_clone(&param_types[k])),
            holds: Vec::new(),
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
    // Types of let-introduced bindings, in source-DFS order. Populated by typeck.
    let_types: Vec<RType>,
    let_idx: usize,
    // Per `MethodCall` expression in source-DFS order, populated by typeck.
    method_resolutions: &'a Vec<MethodResolution>,
    method_idx: usize,
    // Per `Call` expression in source-DFS order, populated by typeck.
    call_resolutions: &'a Vec<CallResolution>,
    call_idx: usize,
    file: String,
    funcs: &'a FuncTable,
    #[allow(dead_code)]
    current_module: &'a Vec<String>,
    #[allow(dead_code)]
    self_target: Option<RType>,
}

struct Holder {
    name: Option<String>,
    rtype: Option<RType>,
    holds: Vec<HeldBorrow>,
}

struct HeldBorrow {
    place: Vec<String>,
    mutable: bool,
}

// A descriptor of the borrows a value carries forward — i.e. which places this
// expression's value, if it's a reference, refers to. For non-reference values,
// always empty. The caller decides what to do with these (absorb into a binding,
// attach to a call slot, drop on the floor).
struct ValueDesc {
    borrows: Vec<HeldBorrow>,
}

fn empty_desc() -> ValueDesc {
    ValueDesc {
        borrows: Vec::new(),
    }
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
        i += 1;
    }
    match &block.tail {
        Some(tail) => walk_expr(state, tail),
        None => Ok(empty_desc()),
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
    let ty = rtype_clone(&state.let_types[state.let_idx]);
    state.let_idx += 1;
    state.holders.push(Holder {
        name: Some(let_stmt.name.clone()),
        rtype: Some(ty),
        holds: desc.borrows,
    });
    Ok(())
}

fn walk_expr(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    match &expr.kind {
        ExprKind::IntLit(_) => Ok(empty_desc()),
        ExprKind::Var(name) => walk_var(state, name, expr),
        ExprKind::Call(call) => walk_call(state, call),
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
        ExprKind::MethodCall(mc) => walk_method_call(state, mc),
    }
}

fn walk_method_call(state: &mut BorrowState, mc: &MethodCall) -> Result<ValueDesc, Error> {
    let res_idx = state.method_idx;
    state.method_idx += 1;
    let recv_adjust = match &state.method_resolutions[res_idx].recv_adjust {
        ReceiverAdjust::Move => RecvAdjustLocal::Move,
        ReceiverAdjust::BorrowImm => RecvAdjustLocal::BorrowImm,
        ReceiverAdjust::BorrowMut => RecvAdjustLocal::BorrowMut,
        ReceiverAdjust::ByRef => RecvAdjustLocal::ByRef,
    };
    let ret_borrows_recv = state.method_resolutions[res_idx].ret_borrows_receiver;
    // Push synthetic call slot.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
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
    // Process remaining args.
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
        i += 1;
    }
    state.holders.truncate(call_idx);
    if ret_borrows_recv {
        Ok(ValueDesc {
            borrows: recv_borrows,
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
        // Reject reads of a ref binding that's been moved.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        let mut i = 0;
        while i < state.moved.len() {
            if paths_share_prefix(&state.moved[i], &place) {
                return Err(Error {
                    file: state.file.clone(),
                    message: format!("`{}` was already moved", place_to_string(&place)),
                    span: expr.span.copy(),
                });
            }
            i += 1;
        }
        if is_mut_ref_holder(&state.holders[idx]) {
            // `&mut T` is non-Copy: reading the binding moves its borrow into
            // whatever consumes it. The binding becomes unusable afterward.
            // (Without this, lexical borrows extend past the binding's last
            // use and shadow any subsequent direct access to the source.)
            let mut taken: Vec<HeldBorrow> = Vec::new();
            std::mem::swap(&mut taken, &mut state.holders[idx].holds);
            state.moved.push(place);
            Ok(ValueDesc { borrows: taken })
        } else {
            // `&T` is Copy: cloning the borrow handle is fine.
            let holds = clone_held_borrows(&state.holders[idx].holds);
            Ok(ValueDesc { borrows: holds })
        }
    } else {
        // Owned read: tracked as a move.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        try_move(state, place, expr.span.copy())?;
        Ok(empty_desc())
    }
}

fn walk_call(state: &mut BorrowState, call: &Call) -> Result<ValueDesc, Error> {
    let res_idx = state.call_idx;
    state.call_idx += 1;
    let ret_ref_source = match &state.call_resolutions[res_idx] {
        CallResolution::Direct(idx) => state.funcs.entries[*idx].ret_ref_source,
        CallResolution::Generic { template_idx, .. } => {
            state.funcs.templates[*template_idx].ret_ref_source
        }
    };

    // Push a synthetic call holder. Borrows produced by argument expressions
    // become its holds for the duration of the call, then the holder is popped.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
    });
    let call_idx = state.holders.len() - 1;
    // Snapshot each arg's borrows before they're absorbed into the call slot,
    // so we can later attach the source arg's borrows to the result desc.
    let mut arg_borrow_snapshots: Vec<Vec<HeldBorrow>> = Vec::new();
    let mut i = 0;
    while i < call.args.len() {
        let desc = walk_expr(state, &call.args[i])?;
        arg_borrow_snapshots.push(clone_held_borrows(&desc.borrows));
        let mut k = 0;
        while k < desc.borrows.len() {
            // Conflict-check the new borrow against every other holder's holds.
            let new = HeldBorrow {
                place: clone_path(&desc.borrows[k].place),
                mutable: desc.borrows[k].mutable,
            };
            check_borrow_conflict(state, &new, &call.args[i].span)?;
            state.holders[call_idx].holds.push(new);
            k += 1;
        }
        i += 1;
    }
    state.holders.truncate(call_idx);
    match ret_ref_source {
        Some(idx) => Ok(ValueDesc {
            borrows: clone_held_borrows(&arg_borrow_snapshots[idx]),
        }),
        None => Ok(empty_desc()),
    }
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
        h += 1;
    }
    Ok(())
}

fn walk_struct_lit(state: &mut BorrowState, lit: &StructLit) -> Result<ValueDesc, Error> {
    // Struct fields can't be references (typeck-rejected), so the constructed
    // struct value carries no borrows. We still walk each field to surface
    // moves / borrow registrations inside their initializer expressions.
    let mut i = 0;
    while i < lit.fields.len() {
        walk_expr(state, &lit.fields[i].value)?;
        i += 1;
    }
    Ok(empty_desc())
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
                // Partial-move out of an owned struct.
                try_move(state, place, expr.span.copy())?;
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
            Ok(ValueDesc { borrows })
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
