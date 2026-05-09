use crate::typeck::{
    EnumTable, MoveStatus, MovedPlace, RType, StructTable, TraitTable, byte_size_of, needs_drop,
};

// Per-binding scope-end drop decision. Combines `is_drop(ty)` with the
// binding's move status from borrowck's `moved_places` snapshot.
//
// `Skip`: not Drop-typed, or moved on every path through the function.
//         Codegen emits no drop call at scope end.
// `Always`: Drop-typed and never moved (Init). Unconditional scope-end
//           drop call.
// `Flagged`: Drop-typed and moved on some-but-not-all paths
//            (MaybeMoved). Codegen allocates a drop flag (i32 wasm
//            local, init=1, cleared to 0 at every move site) and gates
//            the drop call on it.
#[derive(Clone, Copy)]
pub enum DropAction {
    Skip,
    Always,
    Flagged,
}

// Compute the per-binding drop action. Centralizes the
// `needs_drop + moved_places lookup` logic so every callsite (param
// decl, let decl, pattern bind, scope-end emission) makes the same
// decision. Name-based lookup against `moved_places` matches the
// existing `binding_move_status` semantics (single-segment whole-
// binding match). Uses `needs_drop` (not `is_drop`) so aggregates that
// don't themselves impl Drop but contain Drop fields participate in
// drop emission — codegen's walker handles the aggregate-vs-direct-Drop
// dispatch.
pub fn compute_drop_action(
    name: &str,
    ty: &RType,
    moved_places: &Vec<MovedPlace>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
) -> DropAction {
    if !needs_drop(ty, structs, enums, traits) {
        return DropAction::Skip;
    }
    let mut i = 0;
    while i < moved_places.len() {
        if moved_places[i].place.len() == 1 && moved_places[i].place[0] == name {
            return match moved_places[i].status {
                MoveStatus::Moved => DropAction::Skip,
                MoveStatus::MaybeMoved => DropAction::Flagged,
            };
        }
        i += 1;
    }
    // No entry → Init (never moved) → unconditional drop.
    DropAction::Always
}

// Per-binding storage selection. Each binding (param, let value,
// pattern leaf) gets one variant. Pre-decided by `compute_mono_layout`
// so codegen never makes the storage choice itself — it just reads the
// precomputed kind and fills in the wasm-local indices that depend on
// emission order.
//
// `Memory` carries its frame offset (known at layout time).
// `Local` defers to codegen for `wasm_start` and `flat_size` (the
// latter is `flatten_rtype(binding_type).len()` — derivable but
// emission-time convenient).
// `MemoryAt` defers to codegen for the `addr_local` (a wasm i32 local
// allocated at the binding's emission point).
#[derive(Clone, Copy)]
pub enum BindingStorageKind {
    Local,
    Memory { frame_offset: u32 },
    MemoryAt,
}

fn vec_of_false(n: usize) -> Vec<bool> {
    let mut v: Vec<bool> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        v.push(false);
        i += 1;
    }
    v
}



// ============================================================================
// MonoLayout — per-mono storage + drop info keyed by BindingId.
//
// Walks the lowered Mono IR (`MonoBody`). Each MonoLocal has a single
// BindingId; `binding_storage` and `binding_drop_action` are direct vec
// lookups. The escape-analysis side (`binding_addressed`) is computed
// by `walk_block_address` traversing the explicit borrow / autoref /
// index nodes the lowering already inserted — the IR makes every
// address-taking site syntactically visible, so the walker doesn't have
// to infer anything (compare with the older AST-keyed FrameLayout,
// which had to guess at lowering's autoref + auto-deref + Index +
// compound-assign semantics from surface syntax).
// ============================================================================

use crate::mono::{
    BindingId, BindingOrigin, MonoArm, MonoBlock, MonoExpr, MonoExprKind, MonoPlace,
    MonoPlaceKind, MonoStmt, MonoBody,
};

// Per-mono layout decisions keyed by `BindingId` (the post-lowering
// binding identifier from `MonoBody.locals`). Codegen reads
// `binding_storage[binding_id]` directly to pick `Storage::Memory` /
// `MemoryAt` / `Local` for each binding (params, lets, pattern leaves,
// synthesized).
#[allow(dead_code)]
pub struct MonoLayout {
    // Per BindingId: storage decision (Local / Memory{frame_offset} /
    // MemoryAt). Param/Let/Synthesized addressed bindings get Memory;
    // pattern leaves get MemoryAt; everything else is Local.
    pub binding_storage: Vec<BindingStorageKind>,
    // Per BindingId: scope-end drop decision.
    pub binding_drop_action: Vec<DropAction>,
    // Per BindingId: address-taken flag (true if any MonoExpr borrows
    // through this binding's place root, including implicit borrows
    // for autoref method receivers and index-base).
    pub binding_addressed: Vec<bool>,
    // Sum of byte sizes of all addressed bindings — what the function's
    // prologue subtracts from `__sp`.
    pub frame_size: u32,
}

pub fn compute_mono_layout(
    body: &MonoBody,
    moved_places: &Vec<MovedPlace>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
) -> MonoLayout {
    let n = body.locals.len();
    let mut addressed = vec_of_false(n);

    // Phase 1: walk the body and mark bindings whose address is taken.
    walk_block_address(&body.body, &mut addressed);

    // Phase 2: Drop-typed bindings are auto-addressed (need an address
    // for the implicit `Drop::drop(&mut binding)` at scope-end). Uses
    // `needs_drop` so aggregates with Drop fields also get an address
    // — the drop walker computes per-field addresses from the
    // aggregate's base.
    let mut k = 0;
    while k < n {
        if needs_drop(&body.locals[k].ty, structs, enums, traits) {
            addressed[k] = true;
        }
        k += 1;
    }

    // Phase 3: assign storage kinds + frame offsets in BindingId order.
    let mut binding_storage: Vec<BindingStorageKind> = Vec::with_capacity(n);
    let mut frame_size: u32 = 0;
    let mut k = 0;
    while k < n {
        if addressed[k] {
            // Pattern bindings get MemoryAt (codegen allocates an
            // addr_local at bind time, since the address is determined
            // by where the scrutinee lives — not by a fixed frame
            // offset). Synthesized bindings (for-loop's __iter, try-op
            // arm bindings) also use MemoryAt: codegen allocates a
            // dynamic shadow-stack slot at the synth binding's
            // introduction site. Everything else (params, lets) gets
            // Memory{frame_offset} — pre-allocated by the function
            // prologue.
            match body.locals[k].origin {
                BindingOrigin::Pattern(_) | BindingOrigin::Synthesized(_) => {
                    binding_storage.push(BindingStorageKind::MemoryAt);
                }
                _ => {
                    binding_storage.push(BindingStorageKind::Memory {
                        frame_offset: frame_size,
                    });
                    frame_size += byte_size_of(&body.locals[k].ty, structs, enums);
                }
            }
        } else {
            binding_storage.push(BindingStorageKind::Local);
        }
        k += 1;
    }

    // Phase 4: drop action per binding.
    let mut binding_drop_action: Vec<DropAction> = Vec::with_capacity(n);
    let mut k = 0;
    while k < n {
        let action = compute_drop_action(
            &body.locals[k].name,
            &body.locals[k].ty,
            moved_places,
            structs,
            enums,
            traits,
        );
        binding_drop_action.push(action);
        k += 1;
    }

    MonoLayout {
        binding_storage,
        binding_drop_action,
        binding_addressed: addressed,
        frame_size,
    }
}

// Walk a MonoBlock, marking the root binding of every place that's
// borrowed (explicitly or implicitly).
fn walk_block_address(block: &MonoBlock, addressed: &mut Vec<bool>) {
    let mut i = 0;
    while i < block.stmts.len() {
        walk_stmt_address(&block.stmts[i], addressed);
        i += 1;
    }
    if let Some(t) = &block.tail {
        walk_expr_address(t, addressed);
    }
}

fn walk_stmt_address(stmt: &MonoStmt, addressed: &mut Vec<bool>) {
    match stmt {
        MonoStmt::Let { value, .. } => walk_expr_address(value, addressed),
        // Uninit lets have no value to walk; the binding is addressed
        // only if subsequent assignments / borrows demand it.
        MonoStmt::LetUninit { .. } => {}
        MonoStmt::LetPattern { value, .. } => walk_expr_address(value, addressed),
        MonoStmt::Assign { place, value, .. } => {
            walk_place_address(place, addressed);
            walk_expr_address(value, addressed);
        }
        MonoStmt::Expr(e) => walk_expr_address(e, addressed),
        MonoStmt::Drop { binding, .. } => {
            // Drop emits `&mut binding` — the binding is implicitly
            // addressed.
            mark(*binding, addressed);
        }
        MonoStmt::ClearDropFlag { .. } => {}
    }
}

fn walk_expr_address(expr: &MonoExpr, addressed: &mut Vec<bool>) {
    match &expr.kind {
        MonoExprKind::Lit(_) | MonoExprKind::Local(_, _) => {}
        MonoExprKind::PlaceLoad(p) => walk_place_address(p, addressed),
        MonoExprKind::Borrow { place, .. } => {
            // Explicit borrow: mark the place's root binding.
            mark_place_root(place, addressed);
            walk_place_address(place, addressed);
        }
        MonoExprKind::BorrowOfValue { value, .. } => {
            // Materialized into a fresh slot — no binding to mark, but
            // walk for any inner borrows.
            walk_expr_address(value, addressed);
        }
        MonoExprKind::Call { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_address(&args[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::MethodCall { recv, args, recv_adjust, .. } => {
            // BorrowImm/BorrowMut recv_adjust = codegen will autoref
            // the recv. If recv is a place-load (PlaceLoad/Local), its
            // root binding is implicitly addressed.
            use crate::typeck::ReceiverAdjust;
            match recv_adjust {
                ReceiverAdjust::BorrowImm | ReceiverAdjust::BorrowMut => {
                    mark_expr_root_if_place(recv, addressed);
                }
                _ => {}
            }
            walk_expr_address(recv, addressed);
            let mut i = 0;
            while i < args.len() {
                walk_expr_address(&args[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_address(&args[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                walk_expr_address(&fields[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::VariantConstruct { payload, .. } => {
            let mut i = 0;
            while i < payload.len() {
                walk_expr_address(&payload[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                walk_expr_address(&elems[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::Cast { inner, .. } => walk_expr_address(inner, addressed),
        MonoExprKind::Match { scrutinee, arms } => {
            walk_expr_address(scrutinee, addressed);
            walk_arms_address(arms, addressed);
        }
        MonoExprKind::Loop { body, .. } => walk_block_address(body.as_ref(), addressed),
        MonoExprKind::Block(b) | MonoExprKind::Unsafe(b) => {
            walk_block_address(b.as_ref(), addressed);
        }
        MonoExprKind::Break { value, .. } => {
            if let Some(v) = value {
                walk_expr_address(v, addressed);
            }
        }
        MonoExprKind::Continue { .. } => {}
        MonoExprKind::Return { value } => {
            if let Some(v) = value {
                walk_expr_address(v, addressed);
            }
        }
        MonoExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_address(&args[i], addressed);
                i += 1;
            }
        }
        // FnItemAddr: pure constant, no inner exprs to address.
        MonoExprKind::FnItemAddr { .. } => {}
        MonoExprKind::CallIndirect { callee, args, .. } => {
            walk_expr_address(callee, addressed);
            let mut i = 0;
            while i < args.len() {
                walk_expr_address(&args[i], addressed);
                i += 1;
            }
        }
        MonoExprKind::RefDynCoerce { inner_ref, .. } => {
            walk_expr_address(inner_ref, addressed);
        }
        MonoExprKind::DynMethodCall { recv, args, .. } => {
            walk_expr_address(recv, addressed);
            let mut i = 0;
            while i < args.len() {
                walk_expr_address(&args[i], addressed);
                i += 1;
            }
        }
    }
}

fn walk_arms_address(arms: &Vec<MonoArm>, addressed: &mut Vec<bool>) {
    let mut i = 0;
    while i < arms.len() {
        if let Some(g) = &arms[i].guard {
            walk_expr_address(g, addressed);
        }
        walk_expr_address(&arms[i].body, addressed);
        i += 1;
    }
}

fn walk_place_address(place: &MonoPlace, addressed: &mut Vec<bool>) {
    match &place.kind {
        MonoPlaceKind::Local(_) => {}
        MonoPlaceKind::Field { base, .. } | MonoPlaceKind::TupleIndex { base, .. } => {
            walk_place_address(base, addressed);
        }
        MonoPlaceKind::Deref { inner } => walk_expr_address(inner, addressed),
    }
}

// Mark the root binding of a place (Local-rooted Field/TupleIndex
// chain). For Deref-rooted places, no marking — the deref's inner is
// itself an expression, not a binding.
fn mark_place_root(place: &MonoPlace, addressed: &mut Vec<bool>) {
    let mut p = place;
    loop {
        match &p.kind {
            MonoPlaceKind::Local(id) => {
                mark(*id, addressed);
                return;
            }
            MonoPlaceKind::Field { base, .. } | MonoPlaceKind::TupleIndex { base, .. } => {
                p = base;
            }
            MonoPlaceKind::Deref { .. } => return,
        }
    }
}

// If `expr` is a place-form (Local or PlaceLoad), mark its root binding.
fn mark_expr_root_if_place(expr: &MonoExpr, addressed: &mut Vec<bool>) {
    match &expr.kind {
        MonoExprKind::Local(id, _) => mark(*id, addressed),
        MonoExprKind::PlaceLoad(p) => mark_place_root(p, addressed),
        _ => {}
    }
}

fn mark(id: BindingId, addressed: &mut Vec<bool>) {
    let i = id as usize;
    if i < addressed.len() {
        addressed[i] = true;
    }
}
