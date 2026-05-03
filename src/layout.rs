use crate::ast::{Block, Expr, ExprKind, Function, Pattern, PatternKind, Stmt};
use crate::mono::MonoFn;
use crate::typeck::{
    EnumTable, MoveStatus, MovedPlace, RType, StructTable, TraitTable, byte_size_of, is_drop,
};

// Address-taken analysis result. Marks bindings (params + let bindings +
// pattern bindings) whose address is taken anywhere in the body —
// either explicitly (`&binding…`) or implicitly (method receivers,
// indexing, Drop binding scope-end). Addressed bindings are spilled to
// the shadow stack instead of living in flat wasm locals.
pub struct AddressInfo {
    pub param_addressed: Vec<bool>,
    // Indexed by `let_stmt.value.id` (a per-function NodeId). `true` means
    // some `&binding…` chain rooted at that let-binding takes its address;
    // the binding then needs a shadow-stack slot.
    pub let_addressed: Vec<bool>,
    // Indexed by Pattern.id of the binding pattern node (a `Binding` or
    // `At` pattern). Same semantics as `let_addressed` but for match-arm
    // and (later) if-let pattern bindings.
    pub pattern_addressed: Vec<bool>,
}

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
// `is_drop + moved_places lookup` logic so every callsite (param decl,
// let decl, pattern bind, scope-end emission) makes the same decision.
// Name-based lookup against `moved_places` matches the existing
// `binding_move_status` semantics (single-segment whole-binding match).
pub fn compute_drop_action(
    name: &str,
    ty: &RType,
    moved_places: &Vec<MovedPlace>,
    traits: &TraitTable,
) -> DropAction {
    if !is_drop(ty, traits) {
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
// pattern leaf) gets one variant. Pre-decided by `compute_layout` so
// codegen never makes the storage choice itself — it just reads the
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

// Per-mono frame layout: `AddressInfo` plus the byte offsets each
// addressed param/let lives at within the function's shadow-stack
// frame, plus the per-binding storage kind. Computed once per `MonoFn`
// before byte emission begins.
pub struct FrameLayout {
    // Carries the param_addressed/let_addressed/pattern_addressed bools
    // computed during analysis. Codegen consults the per-binding
    // `*_storage` fields below directly; `address_info` is retained so
    // future passes (escape-analysis-aware diagnostics, lints) can
    // query the raw flags without re-running `analyze_addresses`.
    #[allow(dead_code)]
    pub address_info: AddressInfo,
    // For each function param (parallel to `func.params`): the
    // pre-decided storage kind. `Memory { frame_offset }` if addressed,
    // else `Local`.
    pub param_storage: Vec<BindingStorageKind>,
    // Sparse, sized to `func.node_count`, keyed by `let_stmt.value.id`.
    // `Some(kind)` at every let-stmt value-id, `None` elsewhere.
    pub let_storage: Vec<Option<BindingStorageKind>>,
    // Sparse, sized to `func.node_count`, keyed by `Pattern.id` of
    // binding pattern nodes (Binding / At). `Some(MemoryAt)` if the
    // binding is addressed, `Some(Local)` if not, `None` for non-binding
    // pattern nodes (and for binding nodes that aren't reachable in this
    // mono).
    pub pattern_storage: Vec<Option<BindingStorageKind>>,
    // Total bytes the function reserves on the shadow stack at prologue
    // (sum of all addressed param + let sizes).
    pub frame_size: u32,
}

// Top-level entry: compute the full frame layout for one
// monomorphization. Runs after `mono::expand` and before
// `emit_function_concrete`.
pub fn compute_layout(
    mono_fn: &MonoFn,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
) -> FrameLayout {
    let func = mono_fn.func;
    let mut address_info = analyze_addresses(func);
    mark_drop_bindings_addressed(
        func,
        &mono_fn.param_types,
        &mono_fn.expr_types,
        traits,
        &mut address_info,
    );
    // For each `let <destructure> = e;`, if any leaf binding had its
    // address taken (pattern_addressed), also flag `let_addressed` for
    // the value id. The AST tuple-destructure codegen (which spills the
    // whole tuple to a frame slot and gives each leaf a sub-offset
    // Storage::Memory) uses `let_storage[value_id] == Memory` as its
    // frame-spill trigger.
    propagate_pattern_to_let_addressed(&func.body, &mut address_info);

    let node_count = func.node_count as usize;
    let mut frame_size: u32 = 0;
    let mut param_storage: Vec<BindingStorageKind> = Vec::with_capacity(func.params.len());
    let mut k = 0;
    while k < mono_fn.param_types.len() {
        if address_info.param_addressed[k] {
            param_storage.push(BindingStorageKind::Memory { frame_offset: frame_size });
            frame_size += byte_size_of(&mono_fn.param_types[k], structs, enums);
        } else {
            param_storage.push(BindingStorageKind::Local);
        }
        k += 1;
    }
    let mut let_storage: Vec<Option<BindingStorageKind>> = Vec::with_capacity(node_count);
    let mut i = 0;
    while i < node_count {
        let_storage.push(None);
        i += 1;
    }
    let mut order: Vec<u32> = Vec::new();
    collect_let_value_ids(&func.body, &mut order);
    let mut k = 0;
    while k < order.len() {
        let id = order[k] as usize;
        if address_info.let_addressed[id] {
            let_storage[id] = Some(BindingStorageKind::Memory { frame_offset: frame_size });
            let ty = mono_fn.expr_types[id]
                .as_ref()
                .expect("typeck recorded the let's type");
            frame_size += byte_size_of(ty, structs, enums);
        } else {
            let_storage[id] = Some(BindingStorageKind::Local);
        }
        k += 1;
    }
    // Pattern storage: for every pattern Binding/At leaf in the body,
    // record MemoryAt if addressed (per `pattern_addressed`) or Local
    // otherwise. Non-binding pattern nodes stay None.
    let mut pattern_storage: Vec<Option<BindingStorageKind>> = Vec::with_capacity(node_count);
    let mut i = 0;
    while i < node_count {
        pattern_storage.push(None);
        i += 1;
    }
    populate_pattern_storage_block(&func.body, &address_info, &mut pattern_storage);

    FrameLayout {
        address_info,
        param_storage,
        let_storage,
        pattern_storage,
        frame_size,
    }
}

fn populate_pattern_storage_block(
    block: &Block,
    info: &AddressInfo,
    out: &mut Vec<Option<BindingStorageKind>>,
) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(ls) => {
                populate_pattern_storage_pat(&ls.pattern, info, out);
                populate_pattern_storage_expr(&ls.value, info, out);
                if let Some(eb) = &ls.else_block {
                    populate_pattern_storage_block(eb, info, out);
                }
            }
            Stmt::Assign(a) => {
                populate_pattern_storage_expr(&a.lhs, info, out);
                populate_pattern_storage_expr(&a.rhs, info, out);
            }
            Stmt::Expr(e) => populate_pattern_storage_expr(e, info, out),
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(t) = &block.tail {
        populate_pattern_storage_expr(t, info, out);
    }
}

fn populate_pattern_storage_pat(
    pat: &Pattern,
    info: &AddressInfo,
    out: &mut Vec<Option<BindingStorageKind>>,
) {
    let id = pat.id as usize;
    match &pat.kind {
        PatternKind::Binding { .. } => {
            if id < out.len() {
                out[id] = Some(if info.pattern_addressed[id] {
                    BindingStorageKind::MemoryAt
                } else {
                    BindingStorageKind::Local
                });
            }
        }
        PatternKind::At { inner, .. } => {
            if id < out.len() {
                out[id] = Some(if info.pattern_addressed[id] {
                    BindingStorageKind::MemoryAt
                } else {
                    BindingStorageKind::Local
                });
            }
            populate_pattern_storage_pat(inner, info, out);
        }
        PatternKind::Tuple(elems) | PatternKind::VariantTuple { elems, .. } => {
            let mut i = 0;
            while i < elems.len() {
                populate_pattern_storage_pat(&elems[i], info, out);
                i += 1;
            }
        }
        PatternKind::Ref { inner, .. } => populate_pattern_storage_pat(inner, info, out),
        PatternKind::VariantStruct { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                populate_pattern_storage_pat(&fields[i].pattern, info, out);
                i += 1;
            }
        }
        PatternKind::Or(alts) => {
            let mut i = 0;
            while i < alts.len() {
                populate_pattern_storage_pat(&alts[i], info, out);
                i += 1;
            }
        }
        _ => {}
    }
}

fn populate_pattern_storage_expr(
    expr: &Expr,
    info: &AddressInfo,
    out: &mut Vec<Option<BindingStorageKind>>,
) {
    match &expr.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) => {
            populate_pattern_storage_block(b.as_ref(), info, out);
        }
        ExprKind::If(if_expr) => {
            populate_pattern_storage_expr(&if_expr.cond, info, out);
            populate_pattern_storage_block(if_expr.then_block.as_ref(), info, out);
            populate_pattern_storage_block(if_expr.else_block.as_ref(), info, out);
        }
        ExprKind::Match(m) => {
            populate_pattern_storage_expr(&m.scrutinee, info, out);
            let mut i = 0;
            while i < m.arms.len() {
                populate_pattern_storage_pat(&m.arms[i].pattern, info, out);
                if let Some(g) = &m.arms[i].guard {
                    populate_pattern_storage_expr(g, info, out);
                }
                populate_pattern_storage_expr(&m.arms[i].body, info, out);
                i += 1;
            }
        }
        ExprKind::IfLet(il) => {
            populate_pattern_storage_expr(&il.scrutinee, info, out);
            populate_pattern_storage_pat(&il.pattern, info, out);
            populate_pattern_storage_block(il.then_block.as_ref(), info, out);
            populate_pattern_storage_block(il.else_block.as_ref(), info, out);
        }
        ExprKind::While(w) => {
            populate_pattern_storage_expr(&w.cond, info, out);
            populate_pattern_storage_block(w.body.as_ref(), info, out);
        }
        ExprKind::For(f) => {
            populate_pattern_storage_expr(&f.iter, info, out);
            populate_pattern_storage_pat(&f.pattern, info, out);
            populate_pattern_storage_block(f.body.as_ref(), info, out);
        }
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                populate_pattern_storage_expr(&c.args[i], info, out);
                i += 1;
            }
        }
        ExprKind::MethodCall(mc) => {
            populate_pattern_storage_expr(&mc.receiver, info, out);
            let mut i = 0;
            while i < mc.args.len() {
                populate_pattern_storage_expr(&mc.args[i], info, out);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                populate_pattern_storage_expr(&s.fields[i].value, info, out);
                i += 1;
            }
        }
        ExprKind::Builtin { args, .. } | ExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                populate_pattern_storage_expr(&args[i], info, out);
                i += 1;
            }
        }
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                populate_pattern_storage_expr(&elems[i], info, out);
                i += 1;
            }
        }
        ExprKind::FieldAccess(fa) => populate_pattern_storage_expr(&fa.base, info, out),
        ExprKind::TupleIndex { base, .. } => populate_pattern_storage_expr(base, info, out),
        ExprKind::Borrow { inner, .. } | ExprKind::Cast { inner, .. } | ExprKind::Deref(inner) | ExprKind::Try { inner, .. } => {
            populate_pattern_storage_expr(inner, info, out);
        }
        ExprKind::Index { base, index, .. } => {
            populate_pattern_storage_expr(base, info, out);
            populate_pattern_storage_expr(index, info, out);
        }
        ExprKind::Return { value } => {
            if let Some(v) = value {
                populate_pattern_storage_expr(v, info, out);
            }
        }
        ExprKind::IntLit(_) | ExprKind::NegIntLit(_) | ExprKind::StrLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) | ExprKind::Break { .. } | ExprKind::Continue { .. } => {}
    }
}

// Walks the body in source order, appending each `LetStmt`'s value-expr
// NodeId. Frame layout iterates this list to assign offsets in source
// order while keying into NodeId-sized arrays.
fn collect_let_value_ids(block: &Block, out: &mut Vec<u32>) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                collect_lets_in_expr(&let_stmt.value, out);
                out.push(let_stmt.value.id);
            }
            Stmt::Assign(assign) => {
                collect_lets_in_expr(&assign.lhs, out);
                collect_lets_in_expr(&assign.rhs, out);
            }
            Stmt::Expr(expr) => collect_lets_in_expr(expr, out),
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(tail) = &block.tail {
        collect_lets_in_expr(tail, out);
    }
}

fn collect_lets_in_expr(expr: &Expr, out: &mut Vec<u32>) {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::NegIntLit(_) | ExprKind::StrLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => {}
        ExprKind::If(if_expr) => {
            collect_lets_in_expr(&if_expr.cond, out);
            collect_let_value_ids(if_expr.then_block.as_ref(), out);
            collect_let_value_ids(if_expr.else_block.as_ref(), out);
        }
        ExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                collect_lets_in_expr(&args[i], out);
                i += 1;
            }
        }
        ExprKind::Borrow { inner, .. } => collect_lets_in_expr(inner, out),
        ExprKind::Cast { inner, .. } => collect_lets_in_expr(inner, out),
        ExprKind::Deref(inner) => collect_lets_in_expr(inner, out),
        ExprKind::FieldAccess(fa) => collect_lets_in_expr(&fa.base, out),
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                collect_lets_in_expr(&c.args[i], out);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                collect_lets_in_expr(&s.fields[i].value, out);
                i += 1;
            }
        }
        ExprKind::MethodCall(mc) => {
            collect_lets_in_expr(&mc.receiver, out);
            let mut i = 0;
            while i < mc.args.len() {
                collect_lets_in_expr(&mc.args[i], out);
                i += 1;
            }
        }
        ExprKind::Block(b) | ExprKind::Unsafe(b) => collect_let_value_ids(b.as_ref(), out),
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                collect_lets_in_expr(&elems[i], out);
                i += 1;
            }
        }
        ExprKind::TupleIndex { base, .. } => collect_lets_in_expr(base, out),
        ExprKind::Match(m) => {
            collect_lets_in_expr(&m.scrutinee, out);
            let mut i = 0;
            while i < m.arms.len() {
                collect_lets_in_expr(&m.arms[i].body, out);
                i += 1;
            }
        }
        ExprKind::IfLet(il) => {
            collect_lets_in_expr(&il.scrutinee, out);
            collect_let_value_ids(il.then_block.as_ref(), out);
            collect_let_value_ids(il.else_block.as_ref(), out);
        }
        ExprKind::While(w) => {
            collect_lets_in_expr(&w.cond, out);
            collect_let_value_ids(w.body.as_ref(), out);
        }
        ExprKind::For(f) => {
            collect_lets_in_expr(&f.iter, out);
            collect_let_value_ids(f.body.as_ref(), out);
        }
        ExprKind::Break { .. } | ExprKind::Continue { .. } => {}
        ExprKind::Return { value } => {
            if let Some(v) = value {
                collect_lets_in_expr(v, out);
            }
        }
        ExprKind::Try { inner, .. } => collect_lets_in_expr(inner, out),
        ExprKind::Index { base, index, .. } => {
            collect_lets_in_expr(base, out);
            collect_lets_in_expr(index, out);
        }
        ExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                collect_lets_in_expr(&args[i], out);
                i += 1;
            }
        }
    }
}

// ============================================================================
// Drop-bound addressing: bindings whose type is Drop need addresses for
// the implicit `drop(&mut binding)` call at scope-end.
// ============================================================================

fn mark_drop_bindings_addressed(
    func: &Function,
    param_types: &Vec<RType>,
    expr_types: &Vec<Option<RType>>,
    traits: &TraitTable,
    info: &mut AddressInfo,
) {
    let mut i = 0;
    while i < param_types.len() && i < info.param_addressed.len() {
        if is_drop(&param_types[i], traits) {
            info.param_addressed[i] = true;
        }
        i += 1;
    }
    walk_block_drop_marks(&func.body, expr_types, traits, info);
}

fn walk_block_drop_marks(
    block: &Block,
    expr_types: &Vec<Option<RType>>,
    traits: &TraitTable,
    info: &mut AddressInfo,
) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(ls) => {
                let id = ls.value.id as usize;
                if let Some(rt) = &expr_types[id] {
                    if is_drop(rt, traits) {
                        info.let_addressed[id] = true;
                    }
                }
                if let_pattern_has_drop_leaf(&ls.pattern, expr_types, traits) {
                    info.let_addressed[id] = true;
                }
                // Auto-address Drop pattern leaves regardless of let vs
                // let-else. Mono codegen's per-leaf binding path uses
                // `pattern_addressed[leaf_id]` to allocate a dedicated
                // shadow-stack slot per binding (so scope-end drops can
                // emit `drop(&mut leaf)`). The AST tuple-destructure path
                // bypasses `pattern_storage` and addresses leaves via the
                // shared `let_addressed` frame-spill, so this extra flag
                // is harmless for AST and load-bearing for Mono.
                if !matches!(&ls.pattern.kind, PatternKind::Binding { .. } | PatternKind::Wildcard) {
                    auto_address_drop_pattern_bindings(&ls.pattern, expr_types, traits, info);
                }
                walk_expr_drop_marks(&ls.value, expr_types, traits, info);
            }
            Stmt::Assign(a) => {
                walk_expr_drop_marks(&a.lhs, expr_types, traits, info);
                walk_expr_drop_marks(&a.rhs, expr_types, traits, info);
            }
            Stmt::Expr(e) => walk_expr_drop_marks(e, expr_types, traits, info),
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(t) = &block.tail {
        walk_expr_drop_marks(t, expr_types, traits, info);
    }
}

fn let_pattern_has_drop_leaf(
    pat: &Pattern,
    expr_types: &Vec<Option<RType>>,
    traits: &TraitTable,
) -> bool {
    match &pat.kind {
        PatternKind::Binding { .. } | PatternKind::At { .. } => {
            let id = pat.id as usize;
            if let Some(rt) = expr_types.get(id).and_then(|o| o.as_ref()) {
                return is_drop(rt, traits);
            }
            false
        }
        PatternKind::Tuple(elems) | PatternKind::VariantTuple { elems, .. } => {
            let mut i = 0;
            while i < elems.len() {
                if let_pattern_has_drop_leaf(&elems[i], expr_types, traits) {
                    return true;
                }
                i += 1;
            }
            false
        }
        PatternKind::Ref { inner, .. } => let_pattern_has_drop_leaf(inner, expr_types, traits),
        PatternKind::VariantStruct { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                if let_pattern_has_drop_leaf(&fields[i].pattern, expr_types, traits) {
                    return true;
                }
                i += 1;
            }
            false
        }
        _ => false,
    }
}

fn auto_address_drop_pattern_bindings(
    pat: &Pattern,
    expr_types: &Vec<Option<RType>>,
    traits: &TraitTable,
    info: &mut AddressInfo,
) {
    let id = pat.id as usize;
    match &pat.kind {
        PatternKind::Binding { .. } => {
            if let Some(rt) = expr_types.get(id).and_then(|o| o.as_ref()) {
                if is_drop(rt, traits) && id < info.pattern_addressed.len() {
                    info.pattern_addressed[id] = true;
                }
            }
        }
        PatternKind::At { inner, .. } => {
            if let Some(rt) = expr_types.get(id).and_then(|o| o.as_ref()) {
                if is_drop(rt, traits) && id < info.pattern_addressed.len() {
                    info.pattern_addressed[id] = true;
                }
            }
            auto_address_drop_pattern_bindings(inner, expr_types, traits, info);
        }
        PatternKind::Tuple(elems) | PatternKind::VariantTuple { elems, .. } => {
            let mut i = 0;
            while i < elems.len() {
                auto_address_drop_pattern_bindings(&elems[i], expr_types, traits, info);
                i += 1;
            }
        }
        PatternKind::Ref { inner, .. } => {
            auto_address_drop_pattern_bindings(inner, expr_types, traits, info);
        }
        PatternKind::VariantStruct { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                auto_address_drop_pattern_bindings(&fields[i].pattern, expr_types, traits, info);
                i += 1;
            }
        }
        _ => {}
    }
}

fn walk_expr_drop_marks(
    expr: &Expr,
    expr_types: &Vec<Option<RType>>,
    traits: &TraitTable,
    info: &mut AddressInfo,
) {
    match &expr.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) => {
            walk_block_drop_marks(b.as_ref(), expr_types, traits, info);
        }
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                walk_expr_drop_marks(&c.args[i], expr_types, traits, info);
                i += 1;
            }
        }
        ExprKind::MethodCall(mc) => {
            walk_expr_drop_marks(&mc.receiver, expr_types, traits, info);
            let mut i = 0;
            while i < mc.args.len() {
                walk_expr_drop_marks(&mc.args[i], expr_types, traits, info);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                walk_expr_drop_marks(&s.fields[i].value, expr_types, traits, info);
                i += 1;
            }
        }
        ExprKind::FieldAccess(fa) => {
            walk_expr_drop_marks(&fa.base, expr_types, traits, info);
        }
        ExprKind::Borrow { inner, .. } | ExprKind::Deref(inner) => {
            walk_expr_drop_marks(inner, expr_types, traits, info);
        }
        ExprKind::Cast { inner, .. } => {
            walk_expr_drop_marks(inner, expr_types, traits, info);
        }
        ExprKind::IntLit(_) | ExprKind::NegIntLit(_) | ExprKind::StrLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => {}
        ExprKind::If(if_expr) => {
            walk_expr_drop_marks(&if_expr.cond, expr_types, traits, info);
            walk_block_drop_marks(if_expr.then_block.as_ref(), expr_types, traits, info);
            walk_block_drop_marks(if_expr.else_block.as_ref(), expr_types, traits, info);
        }
        ExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_drop_marks(&args[i], expr_types, traits, info);
                i += 1;
            }
        }
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                walk_expr_drop_marks(&elems[i], expr_types, traits, info);
                i += 1;
            }
        }
        ExprKind::TupleIndex { base, .. } => {
            walk_expr_drop_marks(base, expr_types, traits, info);
        }
        ExprKind::Match(m) => {
            walk_expr_drop_marks(&m.scrutinee, expr_types, traits, info);
            let mut i = 0;
            while i < m.arms.len() {
                walk_expr_drop_marks(&m.arms[i].body, expr_types, traits, info);
                i += 1;
            }
        }
        ExprKind::IfLet(il) => {
            walk_expr_drop_marks(&il.scrutinee, expr_types, traits, info);
            walk_block_drop_marks(il.then_block.as_ref(), expr_types, traits, info);
            walk_block_drop_marks(il.else_block.as_ref(), expr_types, traits, info);
        }
        ExprKind::While(w) => {
            walk_expr_drop_marks(&w.cond, expr_types, traits, info);
            walk_block_drop_marks(w.body.as_ref(), expr_types, traits, info);
        }
        ExprKind::For(f) => {
            walk_expr_drop_marks(&f.iter, expr_types, traits, info);
            walk_block_drop_marks(f.body.as_ref(), expr_types, traits, info);
        }
        ExprKind::Break { .. } | ExprKind::Continue { .. } => {}
        ExprKind::Return { value } => {
            if let Some(v) = value {
                walk_expr_drop_marks(v, expr_types, traits, info);
            }
        }
        ExprKind::Try { inner, .. } => {
            walk_expr_drop_marks(inner, expr_types, traits, info);
        }
        ExprKind::Index { base, index, .. } => {
            walk_expr_drop_marks(base, expr_types, traits, info);
            walk_expr_drop_marks(index, expr_types, traits, info);
        }
        ExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_drop_marks(&args[i], expr_types, traits, info);
                i += 1;
            }
        }
    }
}

// ============================================================================
// Address-taken (escape) analysis. Walks the body following binding scopes
// to mark each binding that has its address taken anywhere.
// ============================================================================

fn propagate_pattern_to_let_addressed(block: &Block, info: &mut AddressInfo) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(ls) => {
                if !matches!(&ls.pattern.kind, PatternKind::Binding { .. } | PatternKind::Wildcard)
                    && pattern_has_addressed_leaf(&ls.pattern, info)
                {
                    let id = ls.value.id as usize;
                    if id < info.let_addressed.len() {
                        info.let_addressed[id] = true;
                    }
                }
                propagate_pattern_to_let_addressed_expr(&ls.value, info);
                if let Some(eb) = &ls.else_block {
                    propagate_pattern_to_let_addressed(eb, info);
                }
            }
            Stmt::Assign(a) => {
                propagate_pattern_to_let_addressed_expr(&a.lhs, info);
                propagate_pattern_to_let_addressed_expr(&a.rhs, info);
            }
            Stmt::Expr(e) => propagate_pattern_to_let_addressed_expr(e, info),
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(t) = &block.tail {
        propagate_pattern_to_let_addressed_expr(t, info);
    }
}

fn propagate_pattern_to_let_addressed_expr(expr: &Expr, info: &mut AddressInfo) {
    use crate::ast::ExprKind as K;
    match &expr.kind {
        K::Block(b) | K::Unsafe(b) => propagate_pattern_to_let_addressed(b, info),
        K::If(ie) => {
            propagate_pattern_to_let_addressed_expr(&ie.cond, info);
            propagate_pattern_to_let_addressed(&ie.then_block, info);
            propagate_pattern_to_let_addressed(&ie.else_block, info);
        }
        K::Match(m) => {
            propagate_pattern_to_let_addressed_expr(&m.scrutinee, info);
            let mut i = 0;
            while i < m.arms.len() {
                if let Some(g) = &m.arms[i].guard {
                    propagate_pattern_to_let_addressed_expr(g, info);
                }
                propagate_pattern_to_let_addressed_expr(&m.arms[i].body, info);
                i += 1;
            }
        }
        K::IfLet(il) => {
            propagate_pattern_to_let_addressed_expr(&il.scrutinee, info);
            propagate_pattern_to_let_addressed(&il.then_block, info);
            propagate_pattern_to_let_addressed(&il.else_block, info);
        }
        K::While(w) => {
            propagate_pattern_to_let_addressed_expr(&w.cond, info);
            propagate_pattern_to_let_addressed(&w.body, info);
        }
        K::For(f) => {
            propagate_pattern_to_let_addressed_expr(&f.iter, info);
            propagate_pattern_to_let_addressed(&f.body, info);
        }
        K::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                propagate_pattern_to_let_addressed_expr(&c.args[i], info);
                i += 1;
            }
        }
        K::MethodCall(mc) => {
            propagate_pattern_to_let_addressed_expr(&mc.receiver, info);
            let mut i = 0;
            while i < mc.args.len() {
                propagate_pattern_to_let_addressed_expr(&mc.args[i], info);
                i += 1;
            }
        }
        K::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                propagate_pattern_to_let_addressed_expr(&s.fields[i].value, info);
                i += 1;
            }
        }
        K::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                propagate_pattern_to_let_addressed_expr(&elems[i], info);
                i += 1;
            }
        }
        K::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                propagate_pattern_to_let_addressed_expr(&args[i], info);
                i += 1;
            }
        }
        K::FieldAccess(fa) => propagate_pattern_to_let_addressed_expr(&fa.base, info),
        K::TupleIndex { base, .. } => propagate_pattern_to_let_addressed_expr(base, info),
        K::Cast { inner, .. } => propagate_pattern_to_let_addressed_expr(inner, info),
        K::Borrow { inner, .. } => propagate_pattern_to_let_addressed_expr(inner, info),
        K::Deref(inner) => propagate_pattern_to_let_addressed_expr(inner, info),
        K::Index { base, index, .. } => {
            propagate_pattern_to_let_addressed_expr(base, info);
            propagate_pattern_to_let_addressed_expr(index, info);
        }
        K::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                propagate_pattern_to_let_addressed_expr(&args[i], info);
                i += 1;
            }
        }
        K::Try { inner, .. } => propagate_pattern_to_let_addressed_expr(inner, info),
        K::Return { value: Some(v) } => propagate_pattern_to_let_addressed_expr(v, info),
        _ => {}
    }
}

fn pattern_has_addressed_leaf(pat: &Pattern, info: &AddressInfo) -> bool {
    match &pat.kind {
        PatternKind::Binding { .. } => {
            (pat.id as usize) < info.pattern_addressed.len()
                && info.pattern_addressed[pat.id as usize]
        }
        PatternKind::At { inner, .. } => {
            ((pat.id as usize) < info.pattern_addressed.len()
                && info.pattern_addressed[pat.id as usize])
                || pattern_has_addressed_leaf(inner, info)
        }
        PatternKind::Tuple(elems) | PatternKind::VariantTuple { elems, .. } => {
            elems.iter().any(|p| pattern_has_addressed_leaf(p, info))
        }
        PatternKind::VariantStruct { fields, .. } => {
            fields.iter().any(|f| pattern_has_addressed_leaf(&f.pattern, info))
        }
        PatternKind::Ref { inner, .. } => pattern_has_addressed_leaf(inner, info),
        PatternKind::Or(alts) => alts.iter().any(|p| pattern_has_addressed_leaf(p, info)),
        _ => false,
    }
}

fn analyze_addresses(func: &Function) -> AddressInfo {
    let mut info = AddressInfo {
        param_addressed: vec_of_false(func.params.len()),
        let_addressed: vec_of_false(func.node_count as usize),
        pattern_addressed: vec_of_false(func.node_count as usize),
    };
    let mut stack: Vec<BindingRef> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        stack.push(BindingRef::Param(k, func.params[k].name.clone()));
        k += 1;
    }
    walk_block_addr(&func.body, &mut stack, &mut info);
    info
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

#[derive(Clone)]
enum BindingRef {
    Param(usize, String),
    Let(u32, String),
    Pattern(u32, String),
}

fn binding_ref_name<'a>(b: &'a BindingRef) -> &'a str {
    match b {
        BindingRef::Param(_, n)
        | BindingRef::Let(_, n)
        | BindingRef::Pattern(_, n) => n,
    }
}

fn push_pattern_bindings_for_addr(
    pat: &Pattern,
    value_id: u32,
    stack: &mut Vec<BindingRef>,
) {
    match &pat.kind {
        PatternKind::Binding { name, .. } => {
            // Top-level simple `let mut x = e;`. Push as Let so a
            // subsequent `&x` marks `let_addressed[value_id]`, which
            // both AST and Mono simple-let codegen paths read.
            stack.push(BindingRef::Let(value_id, name.clone()));
        }
        PatternKind::Wildcard => {}
        PatternKind::Tuple(_)
        | PatternKind::VariantTuple { .. }
        | PatternKind::VariantStruct { .. } => {
            // Destructuring let. Push each leaf as a Pattern(pat.id),
            // so per-leaf `&binding` flips `pattern_addressed[leaf_id]`
            // (which the Mono LetPattern path consumes via
            // `pattern_storage`). The AST tuple-destructure path also
            // needs `let_addressed[value_id]` to frame-spill — that's
            // propagated separately by `propagate_pattern_to_let`.
            push_pattern_bindings_addr(pat, stack);
        }
        PatternKind::Ref { inner, .. } | PatternKind::At { inner, .. } => {
            push_pattern_bindings_for_addr(inner, value_id, stack);
        }
        _ => {}
    }
}

fn walk_block_addr(
    block: &Block,
    stack: &mut Vec<BindingRef>,
    info: &mut AddressInfo,
) {
    let mark = stack.len();
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                walk_expr_addr(&let_stmt.value, stack, info);
                push_pattern_bindings_for_addr(&let_stmt.pattern, let_stmt.value.id, stack);
            }
            Stmt::Assign(assign) => {
                walk_expr_addr(&assign.lhs, stack, info);
                walk_expr_addr(&assign.rhs, stack, info);
            }
            Stmt::Expr(expr) => walk_expr_addr(expr, stack, info),
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(tail) = &block.tail {
        walk_expr_addr(tail, stack, info);
    }
    while stack.len() > mark {
        stack.pop();
    }
}

fn mark_root_addressed(stack: &Vec<BindingRef>, root: &str, info: &mut AddressInfo) {
    let mut i = stack.len();
    while i > 0 {
        i -= 1;
        if binding_ref_name(&stack[i]) == root {
            match &stack[i] {
                BindingRef::Param(idx, _) => info.param_addressed[*idx] = true,
                BindingRef::Let(id, _) => info.let_addressed[*id as usize] = true,
                BindingRef::Pattern(id, _) => info.pattern_addressed[*id as usize] = true,
            }
            break;
        }
    }
}

fn walk_expr_addr(
    expr: &Expr,
    stack: &mut Vec<BindingRef>,
    info: &mut AddressInfo,
) {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::NegIntLit(_) | ExprKind::StrLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => {}
        ExprKind::If(if_expr) => {
            walk_expr_addr(&if_expr.cond, stack, info);
            walk_block_addr(if_expr.then_block.as_ref(), stack, info);
            walk_block_addr(if_expr.else_block.as_ref(), stack, info);
        }
        ExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_addr(&args[i], stack, info);
                i += 1;
            }
        }
        ExprKind::Borrow { inner, .. } => {
            if let Some(chain) = extract_place(inner) {
                mark_root_addressed(stack, &chain[0], info);
            }
            walk_expr_addr(inner, stack, info);
        }
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                walk_expr_addr(&c.args[i], stack, info);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                walk_expr_addr(&s.fields[i].value, stack, info);
                i += 1;
            }
        }
        ExprKind::FieldAccess(fa) => {
            walk_expr_addr(&fa.base, stack, info);
        }
        ExprKind::Cast { inner, .. } => walk_expr_addr(inner, stack, info),
        ExprKind::Deref(inner) => walk_expr_addr(inner, stack, info),
        ExprKind::Unsafe(b) => walk_block_addr(b.as_ref(), stack, info),
        ExprKind::Block(b) => walk_block_addr(b.as_ref(), stack, info),
        ExprKind::MethodCall(mc) => {
            // The receiver may be autoref'd at codegen time; that takes
            // its address. Conservatively mark the receiver's root
            // binding as addressed whenever the receiver is a place.
            if let Some(chain) = extract_place(&mc.receiver) {
                mark_root_addressed(stack, &chain[0], info);
            }
            walk_expr_addr(&mc.receiver, stack, info);
            let mut i = 0;
            while i < mc.args.len() {
                walk_expr_addr(&mc.args[i], stack, info);
                i += 1;
            }
        }
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                walk_expr_addr(&elems[i], stack, info);
                i += 1;
            }
        }
        ExprKind::TupleIndex { base, .. } => walk_expr_addr(base, stack, info),
        ExprKind::Match(m) => {
            walk_expr_addr(&m.scrutinee, stack, info);
            let mut i = 0;
            while i < m.arms.len() {
                let mark = stack.len();
                push_pattern_bindings_addr(&m.arms[i].pattern, stack);
                if let Some(g) = &m.arms[i].guard {
                    walk_expr_addr(g, stack, info);
                }
                walk_expr_addr(&m.arms[i].body, stack, info);
                while stack.len() > mark {
                    stack.pop();
                }
                i += 1;
            }
        }
        ExprKind::IfLet(il) => {
            walk_expr_addr(&il.scrutinee, stack, info);
            let mark = stack.len();
            push_pattern_bindings_addr(&il.pattern, stack);
            walk_block_addr(il.then_block.as_ref(), stack, info);
            while stack.len() > mark {
                stack.pop();
            }
            walk_block_addr(il.else_block.as_ref(), stack, info);
        }
        ExprKind::While(w) => {
            walk_expr_addr(&w.cond, stack, info);
            walk_block_addr(w.body.as_ref(), stack, info);
        }
        ExprKind::For(f) => {
            walk_expr_addr(&f.iter, stack, info);
            let mark = stack.len();
            if let PatternKind::Binding { name, .. } = &f.pattern.kind {
                stack.push(BindingRef::Pattern(f.pattern.id, name.clone()));
            }
            walk_block_addr(f.body.as_ref(), stack, info);
            while stack.len() > mark {
                stack.pop();
            }
        }
        ExprKind::Break { .. } | ExprKind::Continue { .. } => {}
        ExprKind::Return { value } => {
            if let Some(v) = value {
                walk_expr_addr(v, stack, info);
            }
        }
        ExprKind::Try { inner, .. } => walk_expr_addr(inner, stack, info),
        ExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_addr(&args[i], stack, info);
                i += 1;
            }
        }
        ExprKind::Index { base, index, .. } => {
            // Indexing implicitly takes `&base` (or `&mut base`) for
            // the Index/IndexMut method call.
            if let Some(chain) = extract_place(base) {
                mark_root_addressed(stack, &chain[0], info);
            }
            walk_expr_addr(base, stack, info);
            walk_expr_addr(index, stack, info);
        }
    }
}

fn push_pattern_bindings_addr(pattern: &Pattern, stack: &mut Vec<BindingRef>) {
    match &pattern.kind {
        PatternKind::Binding { name, .. } => {
            stack.push(BindingRef::Pattern(pattern.id, name.clone()));
        }
        PatternKind::At { name, inner, .. } => {
            stack.push(BindingRef::Pattern(pattern.id, name.clone()));
            push_pattern_bindings_addr(inner, stack);
        }
        PatternKind::Tuple(elems) => {
            let mut k = 0;
            while k < elems.len() {
                push_pattern_bindings_addr(&elems[k], stack);
                k += 1;
            }
        }
        PatternKind::Ref { inner, .. } => push_pattern_bindings_addr(inner, stack),
        PatternKind::VariantTuple { elems, .. } => {
            let mut k = 0;
            while k < elems.len() {
                push_pattern_bindings_addr(&elems[k], stack);
                k += 1;
            }
        }
        PatternKind::VariantStruct { fields, .. } => {
            let mut k = 0;
            while k < fields.len() {
                push_pattern_bindings_addr(&fields[k].pattern, stack);
                k += 1;
            }
        }
        PatternKind::Or(alts) => {
            // All alts bind the same set; walk first.
            if !alts.is_empty() {
                push_pattern_bindings_addr(&alts[0], stack);
            }
        }
        PatternKind::Wildcard
        | PatternKind::LitInt(_)
        | PatternKind::LitBool(_)
        | PatternKind::Range { .. } => {}
    }
}

// Place extraction: `expr` is a chain of `Var` / `FieldAccess` /
// `TupleIndex` rooted at a Var. Returns `[root_name, field1, field2, …]`
// or `None` if `expr` doesn't form a place chain.
pub fn extract_place(expr: &Expr) -> Option<Vec<String>> {
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
            ExprKind::TupleIndex { base, index, .. } => {
                chain.push(format!("{}", index));
                current = base;
            }
            _ => return None,
        }
    }
}

// ============================================================================
// MonoLayout — per-mono storage + drop info keyed by BindingId.
//
// Computed by walking the lowered Mono IR (`MonoBody`) AFTER lowering.
// Replaces FrameLayout once codegen migrates to consume Mono. Unlike
// FrameLayout (which is keyed by NodeId from the AST), this is keyed by
// flat BindingId — each MonoLocal has a single BindingId, and storage /
// drop_action are direct vec lookups.
// ============================================================================

use crate::mono::{
    BindingId, BindingOrigin, MonoArm, MonoBlock, MonoExpr, MonoExprKind, MonoPlace,
    MonoPlaceKind, MonoStmt, MonoBody,
};

// Per-mono layout decisions keyed by `BindingId` (the post-lowering
// binding identifier from `MonoBody.locals`). Computed by
// `compute_mono_layout` and currently *not* consumed by codegen — the
// active codegen path still reads `FrameLayout` (which keys off AST
// `Pattern.id` / `let_stmt.value.id`). The MonoLayout pass is exercised
// on every mono as a smoke test so it stays in sync with `MonoBody`
// shape changes; it'll become the load-bearing layout once the codegen
// transitions to a Mono-IR-only frame model.
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
    // for the implicit `Drop::drop(&mut binding)` at scope-end).
    let mut k = 0;
    while k < n {
        if is_drop(&body.locals[k].ty, traits) {
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
            // addr_local at bind time); everything else gets Memory
            // with a fixed frame offset.
            match body.locals[k].origin {
                BindingOrigin::Pattern(_) => {
                    binding_storage.push(BindingStorageKind::MemoryAt);
                    // No frame slot — the addr_local points wherever
                    // codegen allocates (typically outside the
                    // prologue-reserved frame).
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
            traits,
        );
        binding_drop_action.push(action);
        k += 1;
    }

    let _ = enums; // currently unused — byte_size_of takes structs+enums
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
