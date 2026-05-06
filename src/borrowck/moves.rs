// Move/init dataflow analysis on the CFG.
//
// Forward dataflow over a per-place 3-state lattice (Init implicit /
// MaybeMoved / Moved). At each program point, the analysis tracks
// which places have been moved out of, possibly out of, or are still
// initialized. Worklist iteration to fixed point.
//
// Errors detected: a use of a place whose state is `Moved` or
// `MaybeMoved` (via Move/Copy operand, Borrow target, or as the base of
// a place projection that the program reads from).
//
// What's NOT here: borrow conflicts (NLL — phase 4), liveness (phase
// 3), drop-flag synthesis (phase 5). Those run on top of this pass's
// output (per-block in/out move states).

use super::cfg::{
    BasicBlock, BlockId, Cfg, CfgStmt, CfgStmtKind, LocalId, Operand, OperandKind, Place, Rvalue,
    Terminator,
};
use crate::span::{Error, Span};
use crate::typeck::{is_drop, RType, TraitTable};

#[derive(Clone, PartialEq, Eq)]
pub enum MoveStatus {
    Moved,
    MaybeMoved,
    // Place was declared uninitialized (`let x: T;`) and not yet
    // assigned on the current path. Behaves like `Moved` for the
    // dataflow (reads error, an assignment via `init()` clears it),
    // but produces a distinct "use of uninitialized" diagnostic and
    // — critically — a snapshot of `Uninit` at scope-end means the
    // binding was never in Init memory, so codegen must skip Drop.
    Uninit,
}

#[derive(Clone)]
pub struct MoveSet {
    // Each entry says: the place at `place` is in `status`. A place
    // not present is implicitly Init. Entries are pairwise non-prefix:
    // when a parent place is moved, child entries are removed
    // (subsumed); when a child is moved, the parent stays.
    entries: Vec<(Place, MoveStatus)>,
}

impl MoveSet {
    pub fn empty() -> Self {
        MoveSet {
            entries: Vec::new(),
        }
    }

    fn equal(&self, other: &Self) -> bool {
        if self.entries.len() != other.entries.len() {
            return false;
        }
        let mut i = 0;
        while i < self.entries.len() {
            let (p, s) = &self.entries[i];
            let mut found = false;
            let mut j = 0;
            while j < other.entries.len() {
                if other.entries[j].0 == *p && other.entries[j].1 == *s {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                return false;
            }
            i += 1;
        }
        true
    }

    // Returns Some(status) if `place` (or any of its prefixes/sub-
    // places) is currently moved or maybe-moved. Reads of any such
    // place are errors.
    pub fn check_readable(&self, place: &Place) -> Option<MoveStatus> {
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].0.overlaps(place) {
                return Some(self.entries[i].1.clone());
            }
            i += 1;
        }
        None
    }

    // Mark `place` (and implicitly its children) as moved. Removes any
    // descendant entries that are subsumed; merges with existing entry
    // for the same place by taking max(MaybeMoved, Moved).
    pub fn mark(&mut self, place: Place, status: MoveStatus) {
        // Drop descendants of `place`.
        self.entries.retain(|(p, _)| !place.is_prefix_of(p) || place == *p);
        // Merge or insert.
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].0 == place {
                self.entries[i].1 = max_status(&self.entries[i].1, &status);
                return;
            }
            i += 1;
        }
        self.entries.push((place, status));
    }

    // Re-initialize `place` (assignment): remove `place` and any
    // descendants from the moved set. Children of an ancestor remain
    // (e.g., assigning to `x.f` doesn't initialize `x.g`).
    pub fn init(&mut self, place: &Place) {
        self.entries.retain(|(p, _)| !place.is_prefix_of(p));
    }
}

fn max_status(a: &MoveStatus, b: &MoveStatus) -> MoveStatus {
    if matches!(a, MoveStatus::Moved) || matches!(b, MoveStatus::Moved) {
        MoveStatus::Moved
    } else if matches!(a, MoveStatus::Uninit) && matches!(b, MoveStatus::Uninit) {
        MoveStatus::Uninit
    } else {
        // Mixing Uninit with MaybeMoved (or two MaybeMoveds) yields
        // MaybeMoved — the place is "non-Init on at least one path,"
        // and the maybe-init shape captures that loosely.
        MoveStatus::MaybeMoved
    }
}

// Lattice merge: places present in both with the same status keep that
// status; places present in both with different statuses widen to
// Moved (max); places present in only one side become MaybeMoved (the
// "Init on one path, Moved on the other" case).
pub fn merge(a: &MoveSet, b: &MoveSet) -> MoveSet {
    let mut out: Vec<(Place, MoveStatus)> = Vec::new();
    let mut i = 0;
    while i < a.entries.len() {
        let (pa, sa) = &a.entries[i];
        let mut matched = false;
        let mut j = 0;
        while j < b.entries.len() {
            if b.entries[j].0 == *pa {
                let merged = if sa == &b.entries[j].1 {
                    sa.clone()
                } else {
                    // Different statuses → widen.
                    MoveStatus::MaybeMoved
                };
                out.push((pa.clone(), merged));
                matched = true;
                break;
            }
            j += 1;
        }
        if !matched {
            // Present only in a → MaybeMoved (Init on b's path).
            out.push((pa.clone(), MoveStatus::MaybeMoved));
        }
        i += 1;
    }
    let mut j = 0;
    while j < b.entries.len() {
        let (pb, _) = &b.entries[j];
        let mut found = false;
        let mut k = 0;
        while k < a.entries.len() {
            if a.entries[k].0 == *pb {
                found = true;
                break;
            }
            k += 1;
        }
        if !found {
            out.push((pb.clone(), MoveStatus::MaybeMoved));
        }
        j += 1;
    }
    MoveSet { entries: out }
}

pub struct MoveAnalysis {
    pub block_in: Vec<MoveSet>,
    pub block_out: Vec<MoveSet>,
    // Errors detected during the analysis. Each error has a span
    // pinpointing the problematic use; messages name the place and the
    // status that triggered the error.
    pub errors: Vec<Error>,
    // Drop-flag synthesis output, ready for codegen consumption. For
    // each named binding (local with a name, not a temp) whose final
    // status at scope-end is non-Init, an entry records whether it
    // was definitely Moved or merely MaybeMoved. Init bindings (no
    // entry) drop unconditionally; Moved bindings skip the drop;
    // MaybeMoved bindings drop with a runtime flag that's cleared at
    // each move-site.
    pub moved_locals: Vec<MovedLocal>,
    // Per-move-site annotation: at the AST node `node_id`, the named
    // binding `local_name` was moved out. Codegen uses this to clear
    // the drop flag at the matching codegen point. Only includes whole-
    // binding moves of named bindings (not partial moves, not temps).
    pub move_sites: Vec<(crate::ast::NodeId, String)>,
}

#[derive(Clone)]
pub struct MovedLocal {
    pub local: LocalId,
    pub status: MoveStatus,
}

pub fn analyze(cfg: &Cfg, traits: &TraitTable, file: &str) -> MoveAnalysis {
    let n = cfg.blocks.len();
    let mut block_in: Vec<MoveSet> = (0..n).map(|_| MoveSet::empty()).collect();
    let mut block_out: Vec<MoveSet> = (0..n).map(|_| MoveSet::empty()).collect();
    let mut errors: Vec<Error> = Vec::new();
    // Move-site collection: each Operand::Move whose place is a whole
    // named-binding root contributes one entry per encounter. Final
    // dedup-by-(node_id, name) happens at the end so multiple
    // dataflow iterations don't produce duplicates.
    let mut move_sites_raw: Vec<(crate::ast::NodeId, String)> = Vec::new();

    // Compute predecessors once.
    let preds = compute_predecessors(cfg);

    // Seed the worklist with every block. Forward dataflow needs at
    // least one visit per reachable block, and just seeding the entry
    // doesn't work — the first visit doesn't change `block_out`
    // (empty → empty) so successors never get queued. Seeding all
    // blocks ensures every reachable block gets processed.
    let mut on_work: Vec<bool> = vec![true; n];
    let mut work: Vec<BlockId> = (0..n as BlockId).collect();

    while let Some(b) = work.pop() {
        on_work[b as usize] = false;

        // Compute new in-state: merge predecessors' out-states.
        let new_in = if b == cfg.entry {
            MoveSet::empty()
        } else {
            let mut acc: Option<MoveSet> = None;
            let mut i = 0;
            while i < preds[b as usize].len() {
                let p = preds[b as usize][i];
                let s = block_out[p as usize].clone();
                acc = Some(match acc {
                    Some(prev) => merge(&prev, &s),
                    None => s,
                });
                i += 1;
            }
            acc.unwrap_or_else(MoveSet::empty)
        };
        block_in[b as usize] = new_in.clone();

        // Apply transfer function.
        let mut state = new_in;
        let mut block_errors: Vec<Error> = Vec::new();
        apply_block_transfer(
            &cfg.blocks[b as usize],
            &mut state,
            &mut block_errors,
            &mut move_sites_raw,
            cfg,
            traits,
            file,
        );
        errors.extend(block_errors);

        // If the out-state changed, requeue successors.
        if !block_out[b as usize].equal(&state) {
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

    // Dedup move sites (multiple dataflow iterations might revisit
    // the same operand; only one (node_id, name) pair belongs in the
    // output).
    let mut move_sites: Vec<(crate::ast::NodeId, String)> = Vec::new();
    let mut i = 0;
    while i < move_sites_raw.len() {
        let entry = &move_sites_raw[i];
        if !move_sites
            .iter()
            .any(|e| e.0 == entry.0 && e.1 == entry.1)
        {
            move_sites.push(entry.clone());
        }
        i += 1;
    }

    // Compute moved_locals: for each named binding, look at its move
    // status at the StorageDead point that ends its scope. Whole-
    // binding entries (Place with no projections, root = local) get
    // promoted to MovedLocal.
    let moved_locals = compute_moved_locals(cfg, &block_in, &block_out);

    MoveAnalysis {
        block_in,
        block_out,
        errors,
        moved_locals,
        move_sites,
    }
}

// For each named binding (a non-temp local with a name), find its
// StorageDead point and record the move state at that point.
fn compute_moved_locals(
    cfg: &Cfg,
    block_in: &Vec<MoveSet>,
    block_out: &Vec<MoveSet>,
) -> Vec<MovedLocal> {
    let mut out: Vec<MovedLocal> = Vec::new();
    let n = cfg.blocks.len();
    let mut b = 0;
    while b < n {
        // Walk the block, tracking move state at each statement
        // boundary; when we hit StorageDead(L), capture L's status.
        let mut state = block_in[b].clone();
        let mut i = 0;
        while i < cfg.blocks[b].stmts.len() {
            let stmt = &cfg.blocks[b].stmts[i];
            if let CfgStmtKind::StorageDead(local) = &stmt.kind {
                // Only care about named, non-temp locals.
                if cfg.locals[*local as usize].name.is_some()
                    && !cfg.locals[*local as usize].is_temp
                {
                    let p = Place {
                        root: *local,
                        projections: Vec::new(),
                    };
                    if let Some(status) = state.check_readable(&p) {
                        // Dedup: at most one entry per local across
                        // all StorageDeads (a local should only have
                        // one scope-end, but be defensive).
                        if !out.iter().any(|e| e.local == *local) {
                            out.push(MovedLocal {
                                local: *local,
                                status,
                            });
                        }
                    }
                }
            }
            // Apply the statement's effect on the state.
            apply_stmt_state_only(stmt, &mut state);
            i += 1;
        }
        // Also apply terminator (just in case).
        apply_terminator_state_only(&cfg.blocks[b].terminator, &mut state);
        let _ = block_out;
        b += 1;
    }
    out
}

// Lightweight version of apply_stmt that just updates the state, no
// errors / no move_sites recording. Used by compute_moved_locals
// since we don't need diagnostics during the snapshot pass.
fn apply_stmt_state_only(stmt: &CfgStmt, state: &mut MoveSet) {
    match &stmt.kind {
        CfgStmtKind::Assign { place, rvalue } => {
            apply_rvalue_state_only(rvalue, state);
            state.init(place);
        }
        CfgStmtKind::Uninit(local) => {
            state.mark(
                Place { root: *local, projections: Vec::new() },
                MoveStatus::Uninit,
            );
        }
        _ => {}
    }
}

fn apply_terminator_state_only(term: &Terminator, state: &mut MoveSet) {
    match term {
        Terminator::If { cond, .. } => apply_operand_state_only(cond, state),
        Terminator::SwitchInt { operand, .. } => apply_operand_state_only(operand, state),
        _ => {}
    }
}

fn apply_rvalue_state_only(rv: &Rvalue, state: &mut MoveSet) {
    match rv {
        Rvalue::Use(op) => apply_operand_state_only(op, state),
        Rvalue::Cast { source, .. } => apply_operand_state_only(source, state),
        Rvalue::Call { args, .. }
        | Rvalue::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                apply_operand_state_only(&args[i], state);
                i += 1;
            }
        }
        Rvalue::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                apply_operand_state_only(&fields[i].1, state);
                i += 1;
            }
        }
        Rvalue::Tuple(ops) => {
            let mut i = 0;
            while i < ops.len() {
                apply_operand_state_only(&ops[i], state);
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
                        apply_operand_state_only(&ops[i], state);
                        i += 1;
                    }
                }
                VariantFields::Struct(fields) => {
                    let mut i = 0;
                    while i < fields.len() {
                        apply_operand_state_only(&fields[i].1, state);
                        i += 1;
                    }
                }
            }
        }
        Rvalue::Borrow { .. } | Rvalue::Discriminant(_) => {}
    }
}

fn apply_operand_state_only(op: &Operand, state: &mut MoveSet) {
    if let OperandKind::Move(p) = &op.kind {
        state.mark(p.clone(), MoveStatus::Moved);
    }
}

fn apply_block_transfer(
    block: &BasicBlock,
    state: &mut MoveSet,
    errors: &mut Vec<Error>,
    move_sites: &mut Vec<(crate::ast::NodeId, String)>,
    cfg: &Cfg,
    traits: &TraitTable,
    file: &str,
) {
    let mut i = 0;
    while i < block.stmts.len() {
        apply_stmt(&block.stmts[i], state, errors, move_sites, cfg, traits, file);
        i += 1;
    }
    // Terminator may also read operands.
    apply_terminator_reads(&block.terminator, state, errors, move_sites, cfg, traits, file);
}

fn apply_stmt(
    stmt: &CfgStmt,
    state: &mut MoveSet,
    errors: &mut Vec<Error>,
    move_sites: &mut Vec<(crate::ast::NodeId, String)>,
    cfg: &Cfg,
    traits: &TraitTable,
    file: &str,
) {
    match &stmt.kind {
        CfgStmtKind::Assign { place, rvalue } => {
            apply_rvalue(rvalue, state, errors, move_sites, cfg, traits, file, &stmt.span);
            state.init(place);
        }
        CfgStmtKind::Drop(place) => {
            check_read(state, place, &stmt.span, errors, cfg, file);
        }
        CfgStmtKind::Uninit(local) => {
            state.mark(
                Place { root: *local, projections: Vec::new() },
                MoveStatus::Uninit,
            );
        }
        CfgStmtKind::StorageLive(_) | CfgStmtKind::StorageDead(_) => {}
    }
}

fn apply_rvalue(
    rv: &Rvalue,
    state: &mut MoveSet,
    errors: &mut Vec<Error>,
    move_sites: &mut Vec<(crate::ast::NodeId, String)>,
    cfg: &Cfg,
    traits: &TraitTable,
    file: &str,
    fallback_span: &Span,
) {
    match rv {
        Rvalue::Use(op) => apply_operand(op, state, errors, move_sites, cfg, traits, file),
        Rvalue::Borrow { place, .. } => {
            check_read(state, place, fallback_span, errors, cfg, file);
        }
        Rvalue::Cast { source, .. } => {
            apply_operand(source, state, errors, move_sites, cfg, traits, file)
        }
        Rvalue::Call { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                apply_operand(&args[i], state, errors, move_sites, cfg, traits, file);
                i += 1;
            }
        }
        Rvalue::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                apply_operand(&fields[i].1, state, errors, move_sites, cfg, traits, file);
                i += 1;
            }
        }
        Rvalue::Tuple(ops) => {
            let mut i = 0;
            while i < ops.len() {
                apply_operand(&ops[i], state, errors, move_sites, cfg, traits, file);
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
                        apply_operand(&ops[i], state, errors, move_sites, cfg, traits, file);
                        i += 1;
                    }
                }
                VariantFields::Struct(fields) => {
                    let mut i = 0;
                    while i < fields.len() {
                        apply_operand(&fields[i].1, state, errors, move_sites, cfg, traits, file);
                        i += 1;
                    }
                }
            }
        }
        Rvalue::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                apply_operand(&args[i], state, errors, move_sites, cfg, traits, file);
                i += 1;
            }
        }
        Rvalue::Discriminant(place) => {
            check_read(state, place, fallback_span, errors, cfg, file);
        }
    }
}

fn apply_operand(
    op: &Operand,
    state: &mut MoveSet,
    errors: &mut Vec<Error>,
    move_sites: &mut Vec<(crate::ast::NodeId, String)>,
    cfg: &Cfg,
    traits: &TraitTable,
    file: &str,
) {
    match &op.kind {
        OperandKind::Move(place) => {
            check_read(state, place, &op.span, errors, cfg, file);
            // Move-out-of-borrow check: walk the place's projection
            // chain and reject moves whose path reaches the tail
            // through a `&T` / `&mut T` type. Moving a sub-place
            // reachable only through a borrow would steal data the
            // borrowed-from owner expects to still hold. Typeck used
            // to gate this from inside `check_field_access` but the
            // gate moved here so method receivers (typed in place
            // mode for autoref) don't get rejected for the autoref
            // case while the genuine consuming-self case is still
            // caught.
            //
            // Skip when the path goes through a raw-pointer deref —
            // raw pointers don't carry compile-time ownership info
            // (the `unsafe` block is the soundness boundary), so a
            // move out of `*raw` lands in heap territory rather than
            // the surrounding borrow's region.
            if !is_through_raw_ptr_deref(place, cfg) && move_traverses_borrow(place, cfg) {
                errors.push(Error {
                    file: file.to_string(),
                    message: format!(
                        "cannot move out of borrow: `{}` is reachable only through a reference",
                        place.render(&cfg.locals)
                    ),
                    span: op.span.copy(),
                });
            }
            // Partial-move-of-Drop check: moving a sub-place of a
            // Drop-typed root would leave a hole the destructor can't
            // run over soundly, so it's rejected.
            if !place.projections.is_empty() {
                let root_ty = &cfg.locals[place.root as usize].ty;
                if is_drop(root_ty, traits) {
                    errors.push(Error {
                        file: file.to_string(),
                        message: format!(
                            "cannot move out of `{}`: type implements `Drop`",
                            place.render(&cfg.locals)
                        ),
                        span: op.span.copy(),
                    });
                }
            }
            // Record a move-site if this is a whole-binding move of a
            // named local (= a let-binding or pattern binding, not a
            // temp).
            if place.projections.is_empty() {
                if let Some(nid) = op.node_id {
                    if let Some(name) = &cfg.locals[place.root as usize].name {
                        if !cfg.locals[place.root as usize].is_temp {
                            move_sites.push((nid, name.clone()));
                        }
                    }
                }
            }
            // Don't track moves through raw-pointer derefs. Raw
            // pointers don't carry compile-time ownership info, so
            // `let v = unsafe { *raw };` followed by another use of
            // `raw` (e.g. `raw.cast::<U>()`) is sound from the
            // pointer's perspective — the unsoundness sits in the
            // user's `unsafe` block, not the borrowck's place model.
            // Without this skip, after the deref-move on `*raw` the
            // `raw` ptr itself becomes "moved" via overlap, breaking
            // legitimate raw-pointer manipulation patterns like
            // `Box::into_inner` (which reads T off the heap and
            // then frees the buffer).
            if !is_through_raw_ptr_deref(place, cfg) {
                state.mark(place.clone(), MoveStatus::Moved);
            }
        }
        OperandKind::Copy(place) => {
            check_read(state, place, &op.span, errors, cfg, file);
        }
        OperandKind::ConstInt(_)
        | OperandKind::ConstBool(_)
        | OperandKind::ConstUnit
        | OperandKind::ConstStr(_) => {}
    }
}

fn apply_terminator_reads(
    term: &Terminator,
    state: &mut MoveSet,
    errors: &mut Vec<Error>,
    move_sites: &mut Vec<(crate::ast::NodeId, String)>,
    cfg: &Cfg,
    traits: &TraitTable,
    file: &str,
) {
    match term {
        Terminator::If { cond, .. } => {
            apply_operand(cond, state, errors, move_sites, cfg, traits, file)
        }
        Terminator::SwitchInt { operand, .. } => {
            apply_operand(operand, state, errors, move_sites, cfg, traits, file)
        }
        Terminator::Goto(_) | Terminator::Return | Terminator::Unreachable => {}
    }
}

// True iff `place` projects through a `Deref` and the projection's
// root local has type `*const T` / `*mut T`. Used to suppress
// move-tracking for raw-pointer derefs (raw pointers don't carry
// ownership, so the user's `unsafe { *raw }` shouldn't poison `raw`
// for downstream use).
fn is_through_raw_ptr_deref(place: &super::cfg::Place, cfg: &Cfg) -> bool {
    if !place.projections.iter().any(|p| matches!(p, super::cfg::Projection::Deref)) {
        return false;
    }
    matches!(
        cfg.locals[place.root as usize].ty,
        crate::typeck::RType::RawPtr { .. }
    )
}

// True iff the move's place projects directly off a Ref-typed local
// via a single Field/TupleIndex step, with no further descent. This
// is the case the typeck-side `check_field_access` gate used to
// catch (`r.field` for `r: &T` and non-Copy field type), which got
// lifted to support method receivers needing autoref. We re-add the
// catch here in borrowck for the actual Move case.
//
// Intentionally narrow:
//   - Single projection so we don't have to walk through struct
//     field types (which would require the struct table, not
//     plumbed into this module).
//   - Last step must be Field / TupleIndex — a trailing `Deref` is
//     the raw-pointer-deref pattern (`*self.ptr`) that the existing
//     `is_through_raw_ptr_deref` exempts and that should not be
//     rejected here.
//   - Root must be `Ref`/`RefMut` — moving from inside the borrow's
//     region.
//
// Misses cases like `(*o).p` and `o.x.p` (multi-step). Those weren't
// caught by the prior typeck gate either; this preserves status quo
// for them while plugging the immediate hole opened by typing method
// receivers in place mode.
fn move_traverses_borrow(place: &Place, cfg: &Cfg) -> bool {
    use super::cfg::Projection;
    if place.projections.len() != 1 {
        return false;
    }
    let last = &place.projections[0];
    if !matches!(last, Projection::Field(_) | Projection::TupleIndex(_)) {
        return false;
    }
    matches!(&cfg.locals[place.root as usize].ty, RType::Ref { .. })
}

fn check_read(
    state: &MoveSet,
    place: &Place,
    span: &Span,
    errors: &mut Vec<Error>,
    cfg: &Cfg,
    file: &str,
) {
    if let Some(status) = state.check_readable(place) {
        let rendered = format!("`{}`", place.render(&cfg.locals));
        let msg = match status {
            MoveStatus::Moved => format!("{} was already moved", rendered),
            MoveStatus::MaybeMoved => format!("{} was already moved (on some paths)", rendered),
            MoveStatus::Uninit => format!("use of uninitialized binding {}", rendered),
        };
        errors.push(Error {
            file: file.to_string(),
            message: msg,
            span: span.copy(),
        });
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
