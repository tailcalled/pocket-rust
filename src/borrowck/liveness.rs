// Liveness analysis on the CFG (backward dataflow).
//
// At each program point, computes the set of locals that are "live"
// — i.e., their value will be read on some forward path before being
// overwritten. This drives borrow-region computation in phase 4: a
// borrow is live iff the local holding the reference is live.
//
// Backward worklist: live_out(b) = union of live_in(succ); live_in(b)
// = transfer(live_out, b's stmts in reverse). When a use is seen in a
// statement, the place's root local enters the live set; when a
// whole-local assignment is seen, the local exits.
//
// Granularity: per-LocalId. Field/projection-level liveness would be
// more precise (e.g., `x.f` live without `x.g`) but per-local is
// enough for borrow regions and matches what NLL needs in practice.

use super::cfg::{
    BasicBlock, BlockId, Cfg, CfgStmt, CfgStmtKind, LocalId, Operand, OperandKind, Place, Rvalue,
    Terminator,
};

#[derive(Clone)]
pub struct LiveSet {
    // Sorted set of LocalId. Tiny — most blocks reference few locals.
    locals: Vec<LocalId>,
}

impl LiveSet {
    pub fn empty() -> Self {
        LiveSet { locals: Vec::new() }
    }

    pub fn contains(&self, l: LocalId) -> bool {
        self.locals.binary_search(&l).is_ok()
    }

    pub fn insert(&mut self, l: LocalId) -> bool {
        match self.locals.binary_search(&l) {
            Ok(_) => false,
            Err(idx) => {
                self.locals.insert(idx, l);
                true
            }
        }
    }

    pub fn remove(&mut self, l: LocalId) -> bool {
        match self.locals.binary_search(&l) {
            Ok(idx) => {
                self.locals.remove(idx);
                true
            }
            Err(_) => false,
        }
    }

    pub fn equal(&self, other: &LiveSet) -> bool {
        self.locals == other.locals
    }

    pub fn iter(&self) -> impl Iterator<Item = LocalId> + '_ {
        self.locals.iter().copied()
    }
}

pub fn union(a: &LiveSet, b: &LiveSet) -> LiveSet {
    let mut out = a.clone();
    let mut i = 0;
    while i < b.locals.len() {
        out.insert(b.locals[i]);
        i += 1;
    }
    out
}

pub struct LivenessAnalysis {
    // For each block: the set of locals live at block entry / exit.
    pub block_in: Vec<LiveSet>,
    pub block_out: Vec<LiveSet>,
}

pub fn analyze(cfg: &Cfg) -> LivenessAnalysis {
    let n = cfg.blocks.len();
    let mut block_in: Vec<LiveSet> = (0..n).map(|_| LiveSet::empty()).collect();
    let mut block_out: Vec<LiveSet> = (0..n).map(|_| LiveSet::empty()).collect();

    // Compute predecessors (for requeuing) and seed with all blocks
    // (backward analysis converges from the leaves).
    let preds = compute_predecessors(cfg);

    let mut on_work: Vec<bool> = vec![false; n];
    let mut work: Vec<BlockId> = Vec::new();
    let mut b = 0;
    while b < n {
        work.push(b as BlockId);
        on_work[b] = true;
        b += 1;
    }

    while let Some(b) = work.pop() {
        on_work[b as usize] = false;

        // live_out: union of successors' live_in.
        let new_out = {
            let succs = successors(&cfg.blocks[b as usize].terminator);
            let mut acc = LiveSet::empty();
            let mut i = 0;
            while i < succs.len() {
                acc = union(&acc, &block_in[succs[i] as usize]);
                i += 1;
            }
            acc
        };
        block_out[b as usize] = new_out.clone();

        // Transfer backward through the block.
        let new_in = transfer_block(&cfg.blocks[b as usize], new_out);
        if !new_in.equal(&block_in[b as usize]) {
            block_in[b as usize] = new_in;
            // Requeue predecessors.
            let p = &preds[b as usize];
            let mut i = 0;
            while i < p.len() {
                let q = p[i];
                if !on_work[q as usize] {
                    work.push(q);
                    on_work[q as usize] = true;
                }
                i += 1;
            }
        }
    }

    LivenessAnalysis {
        block_in,
        block_out,
    }
}

fn transfer_block(block: &BasicBlock, mut state: LiveSet) -> LiveSet {
    // Terminator first (it's "after" the statements in execution order;
    // we walk backward).
    transfer_terminator(&block.terminator, &mut state);
    let mut i = block.stmts.len();
    while i > 0 {
        i -= 1;
        transfer_stmt(&block.stmts[i], &mut state);
    }
    state
}

fn transfer_stmt(stmt: &CfgStmt, state: &mut LiveSet) {
    match &stmt.kind {
        CfgStmtKind::Assign { place, rvalue } => {
            // Whole-local assignment kills the local. Field/projection
            // assignments don't kill — they leave other parts alive.
            if place.projections.is_empty() {
                state.remove(place.root);
            }
            // Then add the rvalue's uses (these happen-before the
            // assignment in execution; in backward dataflow they're
            // applied after the kill, so use AFTER kill — but the
            // operand's local may be the same as the assigned place).
            mark_rvalue_uses(rvalue, state);
        }
        CfgStmtKind::Drop(place) => {
            state.insert(place.root);
        }
        CfgStmtKind::StorageLive(_) | CfgStmtKind::StorageDead(_) => {}
    }
}

fn transfer_terminator(term: &Terminator, state: &mut LiveSet) {
    match term {
        Terminator::If { cond, .. } => mark_operand_uses(cond, state),
        Terminator::SwitchInt { operand, .. } => mark_operand_uses(operand, state),
        Terminator::Goto(_) | Terminator::Return | Terminator::Unreachable => {}
    }
}

fn mark_operand_uses(op: &Operand, state: &mut LiveSet) {
    match &op.kind {
        OperandKind::Move(p) | OperandKind::Copy(p) => {
            state.insert(p.root);
        }
        OperandKind::ConstInt(_) | OperandKind::ConstBool(_) | OperandKind::ConstUnit => {}
    }
}

fn mark_rvalue_uses(rv: &Rvalue, state: &mut LiveSet) {
    match rv {
        Rvalue::Use(op) => mark_operand_uses(op, state),
        Rvalue::Borrow { place, .. } => {
            state.insert(place.root);
        }
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
        Rvalue::Discriminant(place) => {
            state.insert(place.root);
        }
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
