// NLL-style borrow checking on the CFG.
//
// Design: forward dataflow over a "set of active borrows" lattice.
// Each borrow rvalue, when executed, adds a borrow descriptor to the
// active set. Before each statement runs, borrows whose destination
// local is no longer live (from phase 3 liveness) are pruned —
// matching NLL's "borrow ends at last use of the reference".
//
// Conflict checks at each statement:
//   - Creating a mutable borrow conflicts with any other active
//     borrow whose place overlaps.
//   - Creating a shared borrow conflicts with any active *mutable*
//     borrow whose place overlaps.
//   - Writing to a place conflicts with any active borrow whose
//     place overlaps.
//   - Moving a place conflicts with any active borrow whose place
//     overlaps.
//
// The "place overlaps" relation is prefix-overlap: two places conflict
// if either is a prefix of the other. So `&x` blocks `&mut x.f`, and
// `&x.f` blocks `&mut x`.

use super::cfg::{
    BasicBlock, BlockId, Cfg, CfgStmt, CfgStmtKind, LocalId, Operand, OperandKind, Place,
    RegionId, Rvalue, Terminator,
};
use super::liveness::LivenessAnalysis;
use crate::span::{Error, Span};

// One active borrow tracked through the dataflow.
#[derive(Clone)]
struct ActiveBorrow {
    id: RegionId,
    place: Place,
    mutable: bool,
    // Local that holds the resulting reference (the assignment's
    // destination). Liveness on this local controls when the borrow
    // expires.
    dest: LocalId,
    // Source location of the borrow rvalue, used for "previous borrow
    // here" notes.
    span: Span,
}

#[derive(Clone)]
struct BorrowSet {
    borrows: Vec<ActiveBorrow>,
}

impl BorrowSet {
    fn empty() -> Self {
        BorrowSet {
            borrows: Vec::new(),
        }
    }

    // Union for dataflow merge.
    fn union_with(&mut self, other: &BorrowSet) {
        let mut i = 0;
        while i < other.borrows.len() {
            let b = &other.borrows[i];
            if !self.borrows.iter().any(|x| x.id == b.id) {
                self.borrows.push(b.clone());
            }
            i += 1;
        }
    }

    fn equal(&self, other: &BorrowSet) -> bool {
        if self.borrows.len() != other.borrows.len() {
            return false;
        }
        let mut i = 0;
        while i < self.borrows.len() {
            if !other.borrows.iter().any(|x| x.id == self.borrows[i].id) {
                return false;
            }
            i += 1;
        }
        true
    }

    // Drop borrows whose destination local is not in the live set.
    fn prune(&mut self, live_locals: &Vec<LocalId>) {
        self.borrows.retain(|b| live_locals.contains(&b.dest));
    }
}

pub struct BorrowCheck {
    pub errors: Vec<Error>,
}

pub fn analyze(cfg: &Cfg, liveness: &LivenessAnalysis, file: &str) -> BorrowCheck {
    let n = cfg.blocks.len();
    let mut block_in: Vec<BorrowSet> = (0..n).map(|_| BorrowSet::empty()).collect();
    let mut block_out: Vec<BorrowSet> = (0..n).map(|_| BorrowSet::empty()).collect();
    let mut errors: Vec<Error> = Vec::new();

    let preds = compute_predecessors(cfg);
    // Seed all blocks (see cfg_moves::analyze for rationale).
    let mut on_work: Vec<bool> = vec![true; n];
    let mut work: Vec<BlockId> = (0..n as BlockId).collect();

    while let Some(b) = work.pop() {
        on_work[b as usize] = false;

        // Merge predecessors.
        let new_in = if b == cfg.entry {
            BorrowSet::empty()
        } else {
            let mut acc = BorrowSet::empty();
            let mut i = 0;
            while i < preds[b as usize].len() {
                let p = preds[b as usize][i];
                acc.union_with(&block_out[p as usize]);
                i += 1;
            }
            acc
        };
        block_in[b as usize] = new_in.clone();

        // Apply transfer through the block.
        let mut state = new_in;
        let mut block_errors: Vec<Error> = Vec::new();
        apply_block(
            &cfg.blocks[b as usize],
            cfg,
            &mut state,
            liveness,
            b,
            &mut block_errors,
            file,
        );
        errors.extend(block_errors);

        if !state.equal(&block_out[b as usize]) {
            block_out[b as usize] = state;
            let succs = successors(&cfg.blocks[b as usize].terminator);
            let mut i = 0;
            while i < succs.len() {
                let s = succs[i];
                if !on_work[s as usize] {
                    work.push(s);
                    on_work[s as usize] = true;
                }
                i += 1;
            }
        }
    }

    BorrowCheck { errors }
}

fn apply_block(
    block: &BasicBlock,
    cfg: &Cfg,
    state: &mut BorrowSet,
    liveness: &LivenessAnalysis,
    block_id: BlockId,
    errors: &mut Vec<Error>,
    file: &str,
) {
    // Compute per-statement live-out by walking backward from the
    // block's live_out (= union of successors' live_in). This gives
    // us, for each statement i, the set of locals live AFTER stmt i
    // executes — which is what we use to prune borrows after each
    // statement.
    let live_after = compute_live_after_per_stmt(block, &liveness.block_out[block_id as usize]);

    let mut i = 0;
    while i < block.stmts.len() {
        apply_stmt(&block.stmts[i], cfg, state, errors, file);
        // Prune borrows whose destination local is now dead.
        state.prune(&live_after[i]);
        i += 1;
    }
}

// For each statement i, compute the set of live locals after stmt i
// runs. live_after[block.stmts.len()] would be the block's live_out;
// live_after[i] is computed by reversing the transfer through stmts
// i+1..N.
fn compute_live_after_per_stmt(
    block: &BasicBlock,
    live_out: &super::liveness::LiveSet,
) -> Vec<Vec<LocalId>> {
    // Start from live_out (state after the last stmt + terminator).
    // We need state after each stmt — i.e., before the next stmt.
    let n = block.stmts.len();
    let mut result: Vec<Vec<LocalId>> = vec![Vec::new(); n];
    // state initially = live_out, then peel back through terminator
    // and stmts to get per-stmt-after states.
    let mut state: Vec<LocalId> = live_out.iter().collect();
    // Apply terminator transfer (backward) — for If/SwitchInt, the
    // operand is read so its local becomes live before the terminator.
    transfer_terminator_backward(&block.terminator, &mut state);
    // After terminator-transfer, state = live BEFORE terminator =
    // live AFTER last stmt. So result[n-1] = state.
    if n > 0 {
        result[n - 1] = state.clone();
        let mut i = n;
        while i > 1 {
            i -= 1;
            transfer_stmt_backward(&block.stmts[i], &mut state);
            result[i - 1] = state.clone();
        }
    }
    result
}

fn transfer_stmt_backward(stmt: &CfgStmt, state: &mut Vec<LocalId>) {
    match &stmt.kind {
        CfgStmtKind::Assign { place, rvalue } => {
            if place.projections.is_empty() {
                state.retain(|x| *x != place.root);
            }
            mark_rvalue_uses(rvalue, state);
        }
        CfgStmtKind::Drop(place) => {
            insert_local(state, place.root);
        }
        CfgStmtKind::StorageLive(_) | CfgStmtKind::StorageDead(_) => {}
    }
}

fn transfer_terminator_backward(term: &Terminator, state: &mut Vec<LocalId>) {
    match term {
        Terminator::If { cond, .. } => mark_operand_uses(cond, state),
        Terminator::SwitchInt { operand, .. } => mark_operand_uses(operand, state),
        _ => {}
    }
}

fn mark_operand_uses(op: &Operand, state: &mut Vec<LocalId>) {
    match &op.kind {
        OperandKind::Move(p) | OperandKind::Copy(p) => insert_local(state, p.root),
        OperandKind::ConstInt(_) | OperandKind::ConstBool(_) | OperandKind::ConstUnit | OperandKind::ConstStr(_) => {}
    }
}

fn mark_rvalue_uses(rv: &Rvalue, state: &mut Vec<LocalId>) {
    match rv {
        Rvalue::Use(op) => mark_operand_uses(op, state),
        Rvalue::Borrow { place, .. } => insert_local(state, place.root),
        Rvalue::Cast { source, .. } => mark_operand_uses(source, state),
        Rvalue::Call { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                mark_operand_uses(&args[i], state);
                i += 1;
            }
        }
        Rvalue::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                mark_operand_uses(&fields[i].1, state);
                i += 1;
            }
        }
        Rvalue::Tuple(ops) => {
            let mut i = 0;
            while i < ops.len() {
                mark_operand_uses(&ops[i], state);
                i += 1;
            }
        }
        Rvalue::Variant { fields, .. } => {
            use super::cfg::VariantFields;
            match fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(ops) => {
                    let mut i = 0;
                    while i < ops.len() {
                        mark_operand_uses(&ops[i], state);
                        i += 1;
                    }
                }
                VariantFields::Struct(fields) => {
                    let mut i = 0;
                    while i < fields.len() {
                        mark_operand_uses(&fields[i].1, state);
                        i += 1;
                    }
                }
            }
        }
        Rvalue::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                mark_operand_uses(&args[i], state);
                i += 1;
            }
        }
        Rvalue::Discriminant(p) => insert_local(state, p.root),
    }
}

fn insert_local(state: &mut Vec<LocalId>, l: LocalId) {
    if !state.contains(&l) {
        state.push(l);
    }
}

fn apply_stmt(stmt: &CfgStmt, cfg: &Cfg, state: &mut BorrowSet, errors: &mut Vec<Error>, file: &str) {
    match &stmt.kind {
        CfgStmtKind::Assign { place, rvalue } => {
            // First check writes/borrows in the rvalue against the
            // active set.
            apply_rvalue(rvalue, place, cfg, state, errors, file, &stmt.span);
            // Then add any borrows produced by the rvalue.
            match rvalue {
                Rvalue::Borrow {
                    mutable,
                    place: borrowed_place,
                    region,
                } => {
                    state.borrows.push(ActiveBorrow {
                        id: *region,
                        place: borrowed_place.clone(),
                        mutable: *mutable,
                        dest: place.root,
                        span: stmt.span.copy(),
                    });
                }
                // Borrow propagation: when the rvalue carries the value
                // of one or more "source" locals (via Use/Cast operands,
                // or Call/StructLit/Tuple/Variant/Builtin args), and the
                // destination has the same root as one of those sources'
                // borrowed locals, duplicate every borrow whose dest is
                // a source local with `dest = place.root`. This keeps
                // borrows alive as long as some live local carries them.
                _ => {
                    // Only propagate borrows when the destination
                    // could plausibly hold a reference. A call returning
                    // a primitive (bool, integer) doesn't carry the
                    // input's borrows even if input args do.
                    let dest_ty = &cfg.locals[place.root as usize].ty;
                    if rtype_contains_ref(dest_ty) {
                        let mut sources: Vec<LocalId> = Vec::new();
                        collect_rvalue_source_locals(rvalue, &mut sources);
                        if !sources.is_empty() {
                            let mut to_add: Vec<ActiveBorrow> = Vec::new();
                            let mut i = 0;
                            while i < state.borrows.len() {
                                let b = &state.borrows[i];
                                if sources.contains(&b.dest) && b.dest != place.root {
                                    to_add.push(ActiveBorrow {
                                        id: b.id,
                                        place: b.place.clone(),
                                        mutable: b.mutable,
                                        dest: place.root,
                                        span: stmt.span.copy(),
                                    });
                                }
                                i += 1;
                            }
                            state.borrows.extend(to_add);
                        }
                    }
                }
            }
            // Writes to a place: if place has any projections (writing
            // to a sub-place), check it doesn't overlap any borrow.
            // For whole-local writes, also check (you can't reassign
            // a borrowed local).
            check_write(place, state, &stmt.span, errors, cfg, file);
        }
        CfgStmtKind::Drop(place) => {
            // Drop is a write — calls destructor, which mutates.
            check_write(place, state, &stmt.span, errors, cfg, file);
        }
        CfgStmtKind::StorageLive(_) | CfgStmtKind::StorageDead(_) => {}
    }
}

// Does this RType carry a reference anywhere in its structure? Refs,
// struct fields with refs, tuple elements with refs all qualify. Used
// by borrow propagation: a call/use whose dest type is purely "owned"
// (bool, integer, owned struct of owned data) can't keep the input's
// borrows alive, so we skip propagation in that case.
fn rtype_contains_ref(t: &crate::typeck::RType) -> bool {
    use crate::typeck::RType;
    match t {
        RType::Ref { .. } => true,
        RType::RawPtr { .. } => false,
        RType::Int(_) | RType::Bool | RType::Param(_) => false,
        RType::Tuple(elems) => elems.iter().any(rtype_contains_ref),
        // For Struct/Enum: type_args + a non-empty lifetime_args (any
        // lifetime parameter implies a ref field somewhere). This is a
        // sound over-approximation — types like `Wrapper<u32>` with no
        // lifetime params correctly fall through.
        RType::Struct {
            type_args,
            lifetime_args,
            ..
        } => !lifetime_args.is_empty() || type_args.iter().any(rtype_contains_ref),
        RType::Enum {
            type_args,
            lifetime_args,
            ..
        } => !lifetime_args.is_empty() || type_args.iter().any(rtype_contains_ref),
        // `[T]` / `str` are unsized; they're only ever observed behind a
        // Ref, which is handled by the Ref arm above. A bare Slice/Str in
        // a value position shouldn't reach here.
        RType::Slice(_) | RType::Str => true,
        // Unconcretized assoc-type projections shouldn't reach borrowck —
        // typeck either resolves them to a concrete type or rejects.
        // Conservative: treat as ref-bearing to avoid over-pruning.
        RType::AssocProj { .. } => true,
        // `!` has no inhabitants — no borrows ever attach to it.
        RType::Never => false,
        // `char` is a 4-byte u32 codepoint — no refs inside.
        RType::Char => false,
    }
}

// Collect every local that contributes its value to this rvalue: the
// place root of every Operand inside, plus the place root for
// Discriminant. Used by borrow propagation to identify which active
// borrows should "ride along" to the rvalue's destination.
fn collect_rvalue_source_locals(rv: &Rvalue, out: &mut Vec<LocalId>) {
    let push_op = |op: &Operand, out: &mut Vec<LocalId>| match &op.kind {
        OperandKind::Move(p) | OperandKind::Copy(p) => {
            if !out.contains(&p.root) {
                out.push(p.root);
            }
        }
        OperandKind::ConstInt(_) | OperandKind::ConstBool(_) | OperandKind::ConstUnit | OperandKind::ConstStr(_) => {}
    };
    match rv {
        Rvalue::Use(op) => push_op(op, out),
        Rvalue::Cast { source, .. } => push_op(source, out),
        Rvalue::Call { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                push_op(&args[i], out);
                i += 1;
            }
        }
        Rvalue::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                push_op(&fields[i].1, out);
                i += 1;
            }
        }
        Rvalue::Tuple(ops) => {
            let mut i = 0;
            while i < ops.len() {
                push_op(&ops[i], out);
                i += 1;
            }
        }
        Rvalue::Variant { fields, .. } => {
            use super::cfg::VariantFields;
            match fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(ops) => {
                    let mut i = 0;
                    while i < ops.len() {
                        push_op(&ops[i], out);
                        i += 1;
                    }
                }
                VariantFields::Struct(fields) => {
                    let mut i = 0;
                    while i < fields.len() {
                        push_op(&fields[i].1, out);
                        i += 1;
                    }
                }
            }
        }
        Rvalue::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                push_op(&args[i], out);
                i += 1;
            }
        }
        Rvalue::Discriminant(_) | Rvalue::Borrow { .. } => {}
    }
}

fn apply_rvalue(
    rv: &Rvalue,
    _dest: &Place,
    cfg: &Cfg,
    state: &mut BorrowSet,
    errors: &mut Vec<Error>,
    file: &str,
    fallback_span: &Span,
) {
    match rv {
        Rvalue::Borrow {
            mutable, place, ..
        } => {
            check_borrow(*mutable, place, state, fallback_span, errors, cfg, file);
        }
        Rvalue::Use(op) => apply_operand_read(op, cfg, state, errors, file),
        Rvalue::Cast { source, .. } => apply_operand_read(source, cfg, state, errors, file),
        Rvalue::Call { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                apply_operand_read(&args[i], cfg, state, errors, file);
                i += 1;
            }
        }
        Rvalue::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                apply_operand_read(&fields[i].1, cfg, state, errors, file);
                i += 1;
            }
        }
        Rvalue::Tuple(ops) => {
            let mut i = 0;
            while i < ops.len() {
                apply_operand_read(&ops[i], cfg, state, errors, file);
                i += 1;
            }
        }
        Rvalue::Variant { fields, .. } => {
            use super::cfg::VariantFields;
            match fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(ops) => {
                    let mut i = 0;
                    while i < ops.len() {
                        apply_operand_read(&ops[i], cfg, state, errors, file);
                        i += 1;
                    }
                }
                VariantFields::Struct(fields) => {
                    let mut i = 0;
                    while i < fields.len() {
                        apply_operand_read(&fields[i].1, cfg, state, errors, file);
                        i += 1;
                    }
                }
            }
        }
        Rvalue::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                apply_operand_read(&args[i], cfg, state, errors, file);
                i += 1;
            }
        }
        Rvalue::Discriminant(place) => {
            // Reading the disc is a shared read.
            check_read_shared(place, state, fallback_span, errors, cfg, file);
        }
    }
}

fn apply_operand_read(op: &Operand, cfg: &Cfg, state: &mut BorrowSet, errors: &mut Vec<Error>, file: &str) {
    match &op.kind {
        OperandKind::Move(p) => check_move(p, state, &op.span, errors, cfg, file),
        OperandKind::Copy(p) => check_read_shared(p, state, &op.span, errors, cfg, file),
        OperandKind::ConstInt(_) | OperandKind::ConstBool(_) | OperandKind::ConstUnit | OperandKind::ConstStr(_) => {}
    }
}

fn check_borrow(
    new_mutable: bool,
    new_place: &Place,
    state: &BorrowSet,
    span: &Span,
    errors: &mut Vec<Error>,
    cfg: &Cfg,
    file: &str,
) {
    let rendered = format!("`{}`", new_place.render(&cfg.locals));
    let mut i = 0;
    while i < state.borrows.len() {
        let b = &state.borrows[i];
        if b.place.overlaps(new_place) {
            // Mutable + anything = conflict; shared + mutable = conflict.
            if new_mutable || b.mutable {
                errors.push(Error {
                    file: file.to_string(),
                    message: if new_mutable && b.mutable {
                        format!("cannot borrow {} as mutable: already borrowed as mutable", rendered)
                    } else if new_mutable {
                        format!("cannot borrow {} as mutable: already borrowed as immutable", rendered)
                    } else {
                        format!("cannot borrow {} as immutable: already borrowed as mutable", rendered)
                    },
                    span: span.copy(),
                });
            }
        }
        i += 1;
    }
}

fn check_write(
    place: &Place,
    state: &BorrowSet,
    span: &Span,
    errors: &mut Vec<Error>,
    cfg: &Cfg,
    file: &str,
) {
    let mut i = 0;
    while i < state.borrows.len() {
        let b = &state.borrows[i];
        if b.place.overlaps(place) {
            errors.push(Error {
                file: file.to_string(),
                message: format!(
                    "cannot assign to `{}` while it is borrowed",
                    place.render(&cfg.locals)
                ),
                span: span.copy(),
            });
            return;
        }
        i += 1;
    }
}

fn check_move(
    place: &Place,
    state: &BorrowSet,
    span: &Span,
    errors: &mut Vec<Error>,
    cfg: &Cfg,
    file: &str,
) {
    let mut i = 0;
    while i < state.borrows.len() {
        let b = &state.borrows[i];
        if b.place.overlaps(place) {
            errors.push(Error {
                file: file.to_string(),
                message: format!(
                    "cannot move `{}` while it is borrowed",
                    place.render(&cfg.locals)
                ),
                span: span.copy(),
            });
            return;
        }
        i += 1;
    }
}

fn check_read_shared(
    place: &Place,
    state: &BorrowSet,
    span: &Span,
    errors: &mut Vec<Error>,
    cfg: &Cfg,
    file: &str,
) {
    // A shared read conflicts only with active mutable borrows.
    let mut i = 0;
    while i < state.borrows.len() {
        let b = &state.borrows[i];
        if b.mutable && b.place.overlaps(place) {
            errors.push(Error {
                file: file.to_string(),
                message: format!(
                    "cannot use `{}` because it is mutably borrowed",
                    place.render(&cfg.locals)
                ),
                span: span.copy(),
            });
            return;
        }
        i += 1;
    }
}

fn successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Goto(b) => vec![*b],
        Terminator::If {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        Terminator::SwitchInt {
            targets,
            otherwise,
            ..
        } => {
            let mut out: Vec<BlockId> = Vec::new();
            let mut i = 0;
            while i < targets.len() {
                out.push(targets[i].1);
                i += 1;
            }
            out.push(*otherwise);
            out
        }
        Terminator::Return | Terminator::Unreachable => Vec::new(),
    }
}

fn compute_predecessors(cfg: &Cfg) -> Vec<Vec<BlockId>> {
    let n = cfg.blocks.len();
    let mut preds: Vec<Vec<BlockId>> = (0..n).map(|_| Vec::new()).collect();
    let mut b = 0;
    while b < n {
        let succs = successors(&cfg.blocks[b].terminator);
        let mut i = 0;
        while i < succs.len() {
            preds[succs[i] as usize].push(b as BlockId);
            i += 1;
        }
        b += 1;
    }
    preds
}
