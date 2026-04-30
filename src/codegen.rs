use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, MethodCall,
    Module, Stmt, StructLit,
};
use crate::span::Error;
use crate::typeck::{
    CallResolution, FuncTable, GenericTemplate, IntKind, MethodResolution, RType, ReceiverAdjust,
    StructTable, byte_size_of, clone_path, flatten_rtype, func_lookup, is_ref_mutable,
    resolve_type, rtype_clone, rtype_eq, struct_lookup, substitute_rtype,
};
use crate::wasm;

// We seed the module with one global at index 0 — the shadow-stack pointer.
const SP_GLOBAL: u32 = 0;

// Tracks monomorphic instantiations of generic templates. Maps each
// (template_idx, concrete type_args) to a wasm function index; queues new ones
// for later emission. Codegen drains the queue after the AST walk.
struct MonoState {
    queue: Vec<MonoWork>,
    map_template: Vec<usize>,
    map_args: Vec<Vec<RType>>,
    map_idx: Vec<u32>,
    next_idx: u32,
}

struct MonoWork {
    template_idx: usize,
    type_args: Vec<RType>,
    wasm_idx: u32,
}

impl MonoState {
    fn new(start_idx: u32) -> Self {
        Self {
            queue: Vec::new(),
            map_template: Vec::new(),
            map_args: Vec::new(),
            map_idx: Vec::new(),
            next_idx: start_idx,
        }
    }

    fn lookup(&self, template_idx: usize, type_args: &Vec<RType>) -> Option<u32> {
        let mut i = 0;
        while i < self.map_template.len() {
            if self.map_template[i] == template_idx
                && rtype_vec_eq(&self.map_args[i], type_args)
            {
                return Some(self.map_idx[i]);
            }
            i += 1;
        }
        None
    }

    fn intern(&mut self, template_idx: usize, type_args: Vec<RType>) -> u32 {
        if let Some(idx) = self.lookup(template_idx, &type_args) {
            return idx;
        }
        let idx = self.next_idx;
        self.next_idx += 1;
        self.map_template.push(template_idx);
        self.map_args.push(rtype_vec_clone(&type_args));
        self.map_idx.push(idx);
        self.queue.push(MonoWork {
            template_idx,
            type_args,
            wasm_idx: idx,
        });
        idx
    }
}

fn rtype_vec_eq(a: &Vec<RType>, b: &Vec<RType>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !rtype_eq(&a[i], &b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

fn make_struct_env(type_params: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), rtype_clone(&type_args[i])));
        i += 1;
    }
    env
}

fn rtype_vec_clone(v: &Vec<RType>) -> Vec<RType> {
    let mut out: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(rtype_clone(&v[i]));
        i += 1;
    }
    out
}

pub fn emit(
    wasm_mod: &mut wasm::Module,
    root: &Module,
    structs: &StructTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut module_path: Vec<String> = Vec::new();
    push_root_name(&mut module_path, root);
    // Monomorphic instantiations get wasm idxs starting after the non-generic
    // entries' idxs (which typeck assigned 0..entries.len()).
    let mut mono = MonoState::new(funcs.entries.len() as u32);
    emit_module(wasm_mod, root, &mut module_path, structs, traits, funcs, &mut mono)?;
    // Drain in FIFO order so wasm_mod.functions index matches the assigned
    // wasm_idx. (Each emit_monomorphic may enqueue more work — those go to the
    // end and are processed after the current batch.)
    while !mono.queue.is_empty() {
        let work = mono.queue.remove(0);
        emit_monomorphic(wasm_mod, work, structs, traits, funcs, &mut mono)?;
    }
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

// ============================================================================
// Storage model
// ============================================================================

// A let/param binding lives either in WASM locals (flat scalars) or in the
// shadow stack at SP+frame_offset. Refs are always Local (just an i32 holding
// an address). Spilled bindings own a fixed byte range in the function's frame.
enum Storage {
    Local { wasm_start: u32, flat_size: u32 },
    Memory { frame_offset: u32 },
}

struct LocalBinding {
    name: String,
    rtype: RType,
    storage: Storage,
}

struct FnCtx<'a> {
    locals: Vec<LocalBinding>,
    next_wasm_local: u32,
    extra_locals: Vec<wasm::ValType>,
    instructions: Vec<wasm::Instruction>,
    structs: &'a StructTable,
    traits: &'a crate::typeck::TraitTable,
    funcs: &'a FuncTable,
    current_module: Vec<String>,
    // Per-NodeId types and resolutions, populated by typeck and substituted
    // through the monomorphization env at emit_function entry. Each consumer
    // (codegen_let_stmt, codegen_call, etc.) looks up by `expr.id`.
    expr_types: Vec<Option<RType>>,
    // Per-let `Some(frame_offset)` if the binding is spilled to the shadow
    // stack, `None` otherwise. Indexed by `let_stmt.value.id`. Sized to
    // `func.node_count`.
    let_offsets: Vec<Option<u32>>,
    method_resolutions: Vec<Option<MethodResolution>>,
    call_resolutions: Vec<Option<CallResolution>>,
    self_target: Option<RType>,
    // T4.6: borrowck's snapshot of every place whose move-state at scope
    // end was non-Init (Moved or MaybeMoved). emit_drops_for_locals_range
    // looks up each Drop binding's name as a single-segment entry — Moved
    // means skip the drop entirely; MaybeMoved (later) means a flagged
    // drop. A binding not present at all is `Init` and drops unconditionally.
    moved_places: Vec<crate::typeck::MovedPlace>,
    // Move-site annotations from borrowck: at each (NodeId, binding-name)
    // pair, the named binding's whole-binding storage is consumed. Used
    // to clear drop flags for MaybeMoved bindings.
    move_sites: Vec<(crate::ast::NodeId, String)>,
    // For each binding name that ended up MaybeMoved at scope-end and
    // is Drop-typed, the wasm local idx of its drop flag (an i32 holding
    // 1=needs-drop, 0=already-moved). Init = 1 at decl, cleared to 0 at
    // each move site, gates the scope-end drop call.
    drop_flags: Vec<(String, u32)>,
    // Multi-value `if` results need a registered FuncType. We don't hold
    // `&mut wasm::Module` directly (would conflict with the post-body
    // `wasm_mod.code.push`); instead we accumulate pending FuncTypes
    // here and append them to `wasm_mod.types` at function-emit-end.
    // `pending_types_base` is `wasm_mod.types.len()` at FnCtx
    // construction, so a pending entry's typeidx is `base + idx_in_pending`.
    pending_types: Vec<wasm::FuncType>,
    pending_types_base: u32,
    // Substitution map for the current monomorphization (empty for non-generic
    // functions). Walks of `Cast` and inner `Generic` callee resolutions apply
    // this env to lower `Param("T")` to concrete RTypes.
    env: Vec<(String, RType)>,
    // Type-param names visible in this function (for `resolve_type` on Cast
    // targets). Empty for non-generic.
    type_params: Vec<String>,
    mono: &'a mut MonoState,
}

// ============================================================================
// Memory layout: leaves
// ============================================================================

// One primitive load/store unit within a value's byte representation. Multi-
// scalar types (structs, u128) flatten into N MemLeafs, in source/declaration
// order — which matches the flat valtype order we already use on the WASM stack.
struct MemLeaf {
    byte_offset: u32,
    byte_size: u32, // 1, 2, 4, or 8
    signed: bool,
    valtype: wasm::ValType,
}

fn collect_leaves(
    rt: &RType,
    structs: &StructTable,
    base_offset: u32,
    out: &mut Vec<MemLeaf>,
) {
    match rt {
        RType::Int(k) => {
            let (size, signed, vt) = int_kind_leaf(k);
            if matches!(k, IntKind::U128 | IntKind::I128) {
                out.push(MemLeaf {
                    byte_offset: base_offset,
                    byte_size: 8,
                    signed: false,
                    valtype: wasm::ValType::I64,
                });
                out.push(MemLeaf {
                    byte_offset: base_offset + 8,
                    byte_size: 8,
                    signed: false,
                    valtype: wasm::ValType::I64,
                });
            } else {
                out.push(MemLeaf {
                    byte_offset: base_offset,
                    byte_size: size,
                    signed,
                    valtype: vt,
                });
            }
        }
        RType::Ref { .. } | RType::RawPtr { .. } => out.push(MemLeaf {
            byte_offset: base_offset,
            byte_size: 4,
            signed: false,
            valtype: wasm::ValType::I32,
        }),
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = make_struct_env(&entry.type_params, type_args);
            let mut off = base_offset;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                collect_leaves(&fty, structs, off, out);
                off += byte_size_of(&fty, structs);
                i += 1;
            }
        }
        RType::Param(_) => {
            unreachable!("collect_leaves on unresolved type parameter — codegen should substitute first");
        }
        RType::Bool => out.push(MemLeaf {
            byte_offset: base_offset,
            byte_size: 1,
            signed: false,
            valtype: wasm::ValType::I32,
        }),
    }
}

fn int_kind_leaf(k: &IntKind) -> (u32, bool, wasm::ValType) {
    match k {
        IntKind::U8 => (1, false, wasm::ValType::I32),
        IntKind::I8 => (1, true, wasm::ValType::I32),
        IntKind::U16 => (2, false, wasm::ValType::I32),
        IntKind::I16 => (2, true, wasm::ValType::I32),
        IntKind::U32 | IntKind::Usize => (4, false, wasm::ValType::I32),
        IntKind::I32 | IntKind::Isize => (4, false, wasm::ValType::I32),
        IntKind::U64 => (8, false, wasm::ValType::I64),
        IntKind::I64 => (8, false, wasm::ValType::I64),
        IntKind::U128 | IntKind::I128 => (16, false, wasm::ValType::I64), // unused by caller
    }
}

fn load_instr(leaf: &MemLeaf, base_offset: u32) -> wasm::Instruction {
    let off = base_offset + leaf.byte_offset;
    match (leaf.byte_size, leaf.signed) {
        (1, false) => wasm::Instruction::I32Load8U { align: 0, offset: off },
        (1, true) => wasm::Instruction::I32Load8S { align: 0, offset: off },
        (2, false) => wasm::Instruction::I32Load16U { align: 0, offset: off },
        (2, true) => wasm::Instruction::I32Load16S { align: 0, offset: off },
        (4, _) => wasm::Instruction::I32Load { align: 0, offset: off },
        (8, _) => wasm::Instruction::I64Load { align: 0, offset: off },
        _ => unreachable!("unexpected leaf size {}", leaf.byte_size),
    }
}

fn store_instr(leaf: &MemLeaf, base_offset: u32) -> wasm::Instruction {
    let off = base_offset + leaf.byte_offset;
    match leaf.byte_size {
        1 => wasm::Instruction::I32Store8 { align: 0, offset: off },
        2 => wasm::Instruction::I32Store16 { align: 0, offset: off },
        4 => wasm::Instruction::I32Store { align: 0, offset: off },
        8 => wasm::Instruction::I64Store { align: 0, offset: off },
        _ => unreachable!("unexpected leaf size {}", leaf.byte_size),
    }
}

// ============================================================================
// Address-taken analysis (escape analysis)
// ============================================================================

struct AddressInfo {
    param_addressed: Vec<bool>,
    // Indexed by `let_stmt.value.id` (a per-function NodeId). `true` means
    // some `&binding…` chain rooted at that let-binding takes its address;
    // the binding then needs a shadow-stack slot.
    let_addressed: Vec<bool>,
}

// T4: walk the function body, marking each `let` whose binding type
// implements Drop as addressed (so it gets a shadow-stack slot). The
// implicit `drop(&mut binding)` call at scope end needs the binding's
// address. T4.5: also marks Drop-typed function parameters so they
// drop at function-body end like any other in-scope Drop binding.
fn mark_drop_bindings_addressed(
    func: &Function,
    param_types: &Vec<RType>,
    expr_types: &Vec<Option<RType>>,
    traits: &crate::typeck::TraitTable,
    info: &mut AddressInfo,
) {
    let mut i = 0;
    while i < param_types.len() && i < info.param_addressed.len() {
        if crate::typeck::is_drop(&param_types[i], traits) {
            info.param_addressed[i] = true;
        }
        i += 1;
    }
    walk_block_drop_marks(&func.body, expr_types, traits, info);
}

fn walk_block_drop_marks(
    block: &Block,
    expr_types: &Vec<Option<RType>>,
    traits: &crate::typeck::TraitTable,
    info: &mut AddressInfo,
) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(ls) => {
                let id = ls.value.id as usize;
                if let Some(rt) = &expr_types[id] {
                    if crate::typeck::is_drop(rt, traits) {
                        info.let_addressed[id] = true;
                    }
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

fn walk_expr_drop_marks(
    expr: &Expr,
    expr_types: &Vec<Option<RType>>,
    traits: &crate::typeck::TraitTable,
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
        ExprKind::IntLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => {}
        ExprKind::If(if_expr) => {
            walk_expr_drop_marks(&if_expr.cond, expr_types, traits, info);
            walk_block_drop_marks(if_expr.then_block.as_ref(), expr_types, traits, info);
            walk_block_drop_marks(if_expr.else_block.as_ref(), expr_types, traits, info);
        }
    }
}

fn analyze_addresses(func: &Function) -> AddressInfo {
    let mut info = AddressInfo {
        param_addressed: vec_of_false(func.params.len()),
        let_addressed: vec_of_false(func.node_count as usize),
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
    // Carries the let's value expr NodeId (used to key let_addressed /
    // let_offsets).
    Let(u32, String),
}

fn binding_ref_name<'a>(b: &'a BindingRef) -> &'a str {
    match b {
        BindingRef::Param(_, n) | BindingRef::Let(_, n) => n,
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
                stack.push(BindingRef::Let(let_stmt.value.id, let_stmt.name.clone()));
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

fn walk_expr_addr(
    expr: &Expr,
    stack: &mut Vec<BindingRef>,
    info: &mut AddressInfo,
) {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => {}
        ExprKind::If(if_expr) => {
            walk_expr_addr(&if_expr.cond, stack, info);
            walk_block_addr(if_expr.then_block.as_ref(), stack, info);
            walk_block_addr(if_expr.else_block.as_ref(), stack, info);
        }
        ExprKind::Borrow { inner, .. } => {
            if let Some(chain) = extract_place(inner) {
                let root = &chain[0];
                let mut i = stack.len();
                while i > 0 {
                    i -= 1;
                    if binding_ref_name(&stack[i]) == root {
                        match &stack[i] {
                            BindingRef::Param(idx, _) => info.param_addressed[*idx] = true,
                            BindingRef::Let(id, _) => info.let_addressed[*id as usize] = true,
                        }
                        break;
                    }
                }
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
            // The receiver may be autoref'd (BorrowImm/BorrowMut) at codegen
            // time; that takes its address. Without consulting typeck's
            // recv_adjust here, conservatively mark the receiver's root binding
            // as addressed whenever the receiver is a place chain. (Same
            // over-approximation as walk_expr_addr for `Borrow`.)
            if let Some(chain) = extract_place(&mc.receiver) {
                let root = &chain[0];
                let mut i = stack.len();
                while i > 0 {
                    i -= 1;
                    if binding_ref_name(&stack[i]) == root {
                        match &stack[i] {
                            BindingRef::Param(idx, _) => info.param_addressed[*idx] = true,
                            BindingRef::Let(id, _) => info.let_addressed[*id as usize] = true,
                        }
                        break;
                    }
                }
            }
            walk_expr_addr(&mc.receiver, stack, info);
            let mut i = 0;
            while i < mc.args.len() {
                walk_expr_addr(&mc.args[i], stack, info);
                i += 1;
            }
        }
    }
}

// ============================================================================
// Module / function emission
// ============================================================================

fn emit_module(
    wasm_mod: &mut wasm::Module,
    module: &Module,
    path: &mut Vec<String>,
    structs: &StructTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                if !f.type_params.is_empty() {
                    // Generic template — skip; emitted lazily via mono queue.
                } else {
                    emit_function(wasm_mod, f, path, path, None, structs, traits, funcs, mono)?;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                emit_module(wasm_mod, m, path, structs, traits, funcs, mono)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Impl(ib) => {
                // Determine the method-path prefix (must mirror what
                // typeck stored in the registered method's path).
                let target_name = match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
                        Some(p.segments[0].name.clone())
                    }
                    _ => None,
                };
                let mut method_prefix = clone_path(path);
                let mut target_path = clone_path(path);
                if let Some(name) = &target_name {
                    method_prefix.push(name.clone());
                    target_path.push(name.clone());
                } else {
                    // Synthetic prefix for non-struct trait targets.
                    // typeck used `__trait_impl_<idx>` — but we don't have
                    // idx here. For now, skip codegen (T2 will route
                    // through the trait impl table anyway).
                    i += 1;
                    continue;
                }
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
                    path: target_path,
                    type_args: impl_param_args,
                    lifetime_args: impl_lifetime_args,
                };
                let impl_is_generic = !ib.type_params.is_empty();
                let mut k = 0;
                while k < ib.methods.len() {
                    let method_is_generic =
                        impl_is_generic || !ib.methods[k].type_params.is_empty();
                    if method_is_generic {
                        // Templated method — emit lazily via mono queue.
                    } else {
                        emit_function(
                            wasm_mod,
                            &ib.methods[k],
                            path,
                            &method_prefix,
                            Some(&target_rt),
                            structs,
                            traits,
                            funcs,
                            mono,
                        )?;
                    }
                    k += 1;
                }
            }
            Item::Trait(_) => {}
            Item::Use(_) => {}
        }
        i += 1;
    }
    Ok(())
}

// Substitutes a template's polymorphic typeck artifacts (which may contain
// `RType::Param`) with concrete types from `type_args`, then dispatches to
// emit_function_concrete with the substituted artifacts.
fn emit_monomorphic(
    wasm_mod: &mut wasm::Module,
    work: MonoWork,
    structs: &StructTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let tmpl = &funcs.templates[work.template_idx];
    let env = build_env(&tmpl.type_params, &work.type_args);
    let param_types = subst_vec(&tmpl.param_types, &env);
    let return_type = tmpl.return_type.as_ref().map(|t| substitute_rtype(t, &env));
    let expr_types = subst_opt_vec(&tmpl.expr_types, &env);
    let method_resolutions = opt_method_resolutions_clone(&tmpl.method_resolutions, &env);
    let call_resolutions = opt_call_resolutions_clone(&tmpl.call_resolutions, &env);
    // Move tracking is independent of concrete type args — the template's
    // snapshot from borrowck applies to every monomorphization.
    let moved_places = clone_moved_places(&tmpl.moved_places);
    let move_sites = clone_move_sites(&tmpl.move_sites);
    // Self target for the body: substitute the impl's stored target pattern
    // through the monomorphization env. This generalizes from the old
    // 1:1-recv-args=impl-args assumption — `impl<T> Pair<usize, T>` produces
    // `Pair<usize, T_concrete>` here.
    let self_target: Option<RType> = tmpl
        .impl_target
        .as_ref()
        .map(|pat| substitute_rtype(pat, &env));
    emit_function_concrete(
        wasm_mod,
        &tmpl.func,
        &tmpl.enclosing_module,
        &tmpl.enclosing_module,
        self_target.as_ref(),
        structs,
        traits,
        funcs,
        mono,
        param_types,
        return_type,
        expr_types,
        method_resolutions,
        call_resolutions,
        moved_places,
        move_sites,
        env,
        tmpl.type_params.clone(),
        work.wasm_idx,
        false, // monomorphic instances are never exported
    )
}

fn subst_opt_vec(v: &Vec<Option<RType>>, env: &Vec<(String, RType)>) -> Vec<Option<RType>> {
    let mut out: Vec<Option<RType>> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        match &v[i] {
            Some(t) => out.push(Some(substitute_rtype(t, env))),
            None => out.push(None),
        }
        i += 1;
    }
    out
}

fn opt_vec_clone(v: &Vec<Option<RType>>) -> Vec<Option<RType>> {
    let mut out: Vec<Option<RType>> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        match &v[i] {
            Some(t) => out.push(Some(rtype_clone(t))),
            None => out.push(None),
        }
        i += 1;
    }
    out
}

fn opt_method_resolutions_clone(
    v: &Vec<Option<MethodResolution>>,
    env: &Vec<(String, RType)>,
) -> Vec<Option<MethodResolution>> {
    let mut out: Vec<Option<MethodResolution>> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        match &v[i] {
            Some(m) => {
                let mut subst_args: Vec<RType> = Vec::new();
                let mut j = 0;
                while j < m.type_args.len() {
                    subst_args.push(substitute_rtype(&m.type_args[j], env));
                    j += 1;
                }
                let trait_dispatch = match &m.trait_dispatch {
                    Some(td) => Some(crate::typeck::TraitDispatch {
                        trait_path: clone_path(&td.trait_path),
                        method_name: td.method_name.clone(),
                        recv_type: substitute_rtype(&td.recv_type, env),
                    }),
                    None => None,
                };
                out.push(Some(MethodResolution {
                    callee_idx: m.callee_idx,
                    callee_path: clone_path(&m.callee_path),
                    recv_adjust: copy_recv_adjust(&m.recv_adjust),
                    ret_borrows_receiver: m.ret_borrows_receiver,
                    template_idx: m.template_idx,
                    type_args: subst_args,
                    trait_dispatch,
                }));
            }
            None => out.push(None),
        }
        i += 1;
    }
    out
}

fn opt_call_resolutions_clone(
    v: &Vec<Option<CallResolution>>,
    env: &Vec<(String, RType)>,
) -> Vec<Option<CallResolution>> {
    let mut out: Vec<Option<CallResolution>> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        match &v[i] {
            Some(CallResolution::Direct(idx)) => out.push(Some(CallResolution::Direct(*idx))),
            Some(CallResolution::Generic { template_idx, type_args }) => {
                out.push(Some(CallResolution::Generic {
                    template_idx: *template_idx,
                    type_args: subst_vec(type_args, env),
                }));
            }
            None => out.push(None),
        }
        i += 1;
    }
    out
}

fn build_env(type_params: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < type_params.len() {
        env.push((type_params[i].clone(), rtype_clone(&type_args[i])));
        i += 1;
    }
    env
}

fn subst_vec(v: &Vec<RType>, env: &Vec<(String, RType)>) -> Vec<RType> {
    let mut out: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(substitute_rtype(&v[i], env));
        i += 1;
    }
    out
}

// Walks the body in source order, appending each `LetStmt`'s value-expr
// NodeId. Frame layout iterates this list to assign offsets in source order
// while keying into NodeId-sized arrays.
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
        ExprKind::IntLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => {}
        ExprKind::If(if_expr) => {
            collect_lets_in_expr(&if_expr.cond, out);
            collect_let_value_ids(if_expr.then_block.as_ref(), out);
            collect_let_value_ids(if_expr.else_block.as_ref(), out);
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
    }
}

fn copy_recv_adjust(r: &ReceiverAdjust) -> ReceiverAdjust {
    match r {
        ReceiverAdjust::Move => ReceiverAdjust::Move,
        ReceiverAdjust::BorrowImm => ReceiverAdjust::BorrowImm,
        ReceiverAdjust::BorrowMut => ReceiverAdjust::BorrowMut,
        ReceiverAdjust::ByRef => ReceiverAdjust::ByRef,
    }
}

fn emit_function(
    wasm_mod: &mut wasm::Module,
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    structs: &StructTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let mut full = clone_path(path_prefix);
    full.push(func.name.clone());
    let entry = func_lookup(funcs, &full).expect("typeck registered this function");
    // Snapshot all artifacts before entering the concrete emitter (which takes
    // them by-value). For non-generic fns these are the entry's data; the env
    // is empty (no Param substitution to do).
    let param_types = rtype_vec_clone(&entry.param_types);
    let return_type = entry.return_type.as_ref().map(rtype_clone);
    let expr_types = opt_vec_clone(&entry.expr_types);
    let method_resolutions = opt_method_resolutions_clone(&entry.method_resolutions, &Vec::new());
    let call_resolutions = opt_call_resolutions_clone(&entry.call_resolutions, &Vec::new());
    let moved_places = clone_moved_places(&entry.moved_places);
    let move_sites = clone_move_sites(&entry.move_sites);
    let wasm_idx = entry.idx;
    let is_export = current_module.is_empty() && path_prefix.len() == current_module.len();
    emit_function_concrete(
        wasm_mod,
        func,
        current_module,
        path_prefix,
        self_target,
        structs,
        traits,
        funcs,
        mono,
        param_types,
        return_type,
        expr_types,
        method_resolutions,
        call_resolutions,
        moved_places,
        move_sites,
        Vec::new(),
        Vec::new(),
        wasm_idx,
        is_export,
    )
}

fn clone_moved_places(v: &Vec<crate::typeck::MovedPlace>) -> Vec<crate::typeck::MovedPlace> {
    let mut out: Vec<crate::typeck::MovedPlace> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(v[i].clone());
        i += 1;
    }
    out
}

fn clone_move_sites(
    v: &Vec<(crate::ast::NodeId, String)>,
) -> Vec<(crate::ast::NodeId, String)> {
    let mut out: Vec<(crate::ast::NodeId, String)> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push((v[i].0, v[i].1.clone()));
        i += 1;
    }
    out
}

fn emit_function_concrete(
    wasm_mod: &mut wasm::Module,
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    structs: &StructTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
    param_types: Vec<RType>,
    return_type: Option<RType>,
    expr_types: Vec<Option<RType>>,
    method_resolutions: Vec<Option<MethodResolution>>,
    call_resolutions: Vec<Option<CallResolution>>,
    moved_places: Vec<crate::typeck::MovedPlace>,
    move_sites: Vec<(crate::ast::NodeId, String)>,
    env: Vec<(String, RType)>,
    type_params: Vec<String>,
    wasm_idx: u32,
    is_export: bool,
) -> Result<(), Error> {
    let _ = path_prefix;
    let node_count = func.node_count as usize;
    // Address-taken analysis: who needs to live in shadow-stack memory?
    let mut address_info = analyze_addresses(func);
    // T4: Drop bindings need to be addressable so the implicit drop call
    // at scope end can pass `&mut binding`. Force-mark them as addressed
    // before frame layout.
    mark_drop_bindings_addressed(func, &param_types, &expr_types, traits, &mut address_info);

    // Compute frame layout: assign byte offsets to addressed params + lets.
    let mut frame_size: u32 = 0;
    let mut param_offsets: Vec<Option<u32>> = Vec::with_capacity(func.params.len());
    let mut k = 0;
    while k < param_types.len() {
        if address_info.param_addressed[k] {
            param_offsets.push(Some(frame_size));
            frame_size += byte_size_of(&param_types[k], structs);
        } else {
            param_offsets.push(None);
        }
        k += 1;
    }
    // let_offsets keyed by let_stmt.value.id — sparse, sized to node_count.
    let mut let_offsets: Vec<Option<u32>> = Vec::with_capacity(node_count);
    let mut i = 0;
    while i < node_count {
        let_offsets.push(None);
        i += 1;
    }
    {
        let mut order: Vec<u32> = Vec::new();
        collect_let_value_ids(&func.body, &mut order);
        let mut k = 0;
        while k < order.len() {
            let id = order[k] as usize;
            if address_info.let_addressed[id] {
                let_offsets[id] = Some(frame_size);
                let ty = expr_types[id]
                    .as_ref()
                    .expect("typeck recorded the let's type");
                frame_size += byte_size_of(ty, structs);
            }
            k += 1;
        }
    }

    // Build the WASM signature: refs collapse to a single i32; everything else
    // flattens to flat scalars as before.
    let mut wasm_params: Vec<wasm::ValType> = Vec::new();
    let mut next_wasm_local: u32 = 0;
    let mut locals: Vec<LocalBinding> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let pty = rtype_clone(&param_types[k]);
        let storage = match &param_offsets[k] {
            Some(off) => Storage::Memory { frame_offset: *off },
            None => {
                let mut vts: Vec<wasm::ValType> = Vec::new();
                flatten_rtype(&pty, structs, &mut vts);
                let start = next_wasm_local;
                let mut j = 0;
                while j < vts.len() {
                    wasm_params.push(vts[j].copy());
                    next_wasm_local += 1;
                    j += 1;
                }
                Storage::Local {
                    wasm_start: start,
                    flat_size: vts.len() as u32,
                }
            }
        };
        if let Some(_) = &param_offsets[k] {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&pty, structs, &mut vts);
            let mut j = 0;
            while j < vts.len() {
                wasm_params.push(vts[j].copy());
                next_wasm_local += 1;
                j += 1;
            }
        }
        locals.push(LocalBinding {
            name: func.params[k].name.clone(),
            rtype: pty,
            storage,
        });
        k += 1;
    }

    let mut wasm_results: Vec<wasm::ValType> = Vec::new();
    if let Some(rt) = &return_type {
        flatten_rtype(rt, structs, &mut wasm_results);
    }

    let func_type = wasm::FuncType {
        params: wasm_params,
        results: wasm_results,
    };
    let type_idx = wasm_mod.types.len() as u32;
    wasm_mod.types.push(func_type);

    let func_idx = wasm_idx;
    wasm_mod.functions.push(type_idx);

    let mut ctx = FnCtx {
        locals,
        next_wasm_local,
        extra_locals: Vec::new(),
        instructions: Vec::new(),
        structs,
        traits,
        funcs,
        current_module: clone_path(current_module),
        expr_types,
        let_offsets,
        method_resolutions,
        call_resolutions,
        self_target: self_target.map(|rt| rtype_clone(rt)),
        moved_places,
        move_sites,
        drop_flags: Vec::new(),
        pending_types: Vec::new(),
        pending_types_base: wasm_mod.types.len() as u32,
        env,
        type_params,
        mono,
    };

    // Prologue: SP -= frame_size; copy spilled params from their incoming
    // WASM-local slots into shadow-stack memory.
    if frame_size > 0 {
        ctx.instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions
            .push(wasm::Instruction::I32Const(frame_size as i32));
        ctx.instructions.push(wasm::Instruction::I32Sub);
        ctx.instructions
            .push(wasm::Instruction::GlobalSet(SP_GLOBAL));

        // Copy each spilled param from its incoming WASM-local slot into memory.
        // Scan locals[] in declaration order — params are first, in order.
        let mut p = 0;
        let mut wasm_local_cursor: u32 = 0;
        while p < func.params.len() {
            let pty = rtype_clone(&ctx.locals[p].rtype);
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&pty, structs, &mut vts);
            let flat_size = vts.len() as u32;
            match &param_offsets[p] {
                Some(off) => {
                    let mut leaves: Vec<MemLeaf> = Vec::new();
                    collect_leaves(&pty, structs, 0, &mut leaves);
                    let mut k = 0;
                    while k < leaves.len() {
                        ctx.instructions
                            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
                        ctx.instructions
                            .push(wasm::Instruction::LocalGet(wasm_local_cursor + k as u32));
                        ctx.instructions.push(store_instr(&leaves[k], *off));
                        k += 1;
                    }
                }
                None => {}
            }
            wasm_local_cursor += flat_size;
            p += 1;
        }
    }

    // Allocate drop flags for any param that's MaybeMoved at scope-end.
    // Init = 1 (param always initialized at fn entry).
    let mut p = 0;
    while p < func.params.len() {
        let local_name = ctx.locals[p].name.clone();
        let rt = rtype_clone(&ctx.locals[p].rtype);
        if needs_drop_flag(&ctx.moved_places, &local_name, &rt, ctx.traits) {
            let flag_idx = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.drop_flags.push((local_name, flag_idx));
            ctx.instructions.push(wasm::Instruction::I32Const(1));
            ctx.instructions.push(wasm::Instruction::LocalSet(flag_idx));
        }
        p += 1;
    }

    codegen_block(&mut ctx, &func.body)?;

    // T4: drop in-scope Drop-typed bindings at function-body end. The
    // tail value (if any) is on the WASM stack — save it to fresh
    // locals, emit drops, then reload, so the return value survives the
    // drop calls.
    let return_flat: Vec<wasm::ValType> = match &return_type {
        Some(rt) => {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(rt, structs, &mut vts);
            vts
        }
        None => Vec::new(),
    };
    if !return_flat.is_empty() {
        let save_start = ctx.next_wasm_local;
        let mut i = 0;
        while i < return_flat.len() {
            ctx.extra_locals.push(return_flat[i].copy());
            ctx.next_wasm_local += 1;
            i += 1;
        }
        // Pop in reverse (top of stack → last local).
        let mut k = return_flat.len();
        while k > 0 {
            k -= 1;
            ctx.instructions
                .push(wasm::Instruction::LocalSet(save_start + k as u32));
        }
        let n = ctx.locals.len();
        emit_drops_for_locals_range(&mut ctx, 0, n)?;
        let mut k = 0;
        while k < return_flat.len() {
            ctx.instructions
                .push(wasm::Instruction::LocalGet(save_start + k as u32));
            k += 1;
        }
    } else {
        let n = ctx.locals.len();
        emit_drops_for_locals_range(&mut ctx, 0, n)?;
    }

    // Epilogue: SP += frame_size. The return value (if any) is already on the
    // WASM stack from the body's tail expression; SP arithmetic doesn't touch
    // the operand stack.
    if frame_size > 0 {
        ctx.instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions
            .push(wasm::Instruction::I32Const(frame_size as i32));
        ctx.instructions.push(wasm::Instruction::I32Add);
        ctx.instructions
            .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    }

    // Drain any FuncTypes that codegen registered for multi-value if-blocks.
    // They were assigned typeidx = pending_types_base + position, so appending
    // them now (in order) makes those typeidxs correct in the final module.
    while !ctx.pending_types.is_empty() {
        wasm_mod.types.push(ctx.pending_types.remove(0));
    }
    let body = wasm::FuncBody {
        locals: ctx.extra_locals,
        instructions: ctx.instructions,
    };
    wasm_mod.code.push(body);

    if is_export {
        wasm_mod.exports.push(wasm::Export {
            name: func.name.clone(),
            kind: wasm::ExportKind::Func,
            index: func_idx,
        });
    }

    Ok(())
}

// ============================================================================
// Statement / expression codegen
// ============================================================================

fn codegen_block(ctx: &mut FnCtx, block: &Block) -> Result<(), Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => codegen_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => codegen_assign_stmt(ctx, assign)?,
            Stmt::Expr(expr) => codegen_expr_stmt(ctx, expr)?,
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(expr) = &block.tail {
        codegen_expr(ctx, expr)?;
    }
    Ok(())
}

fn codegen_expr_stmt(ctx: &mut FnCtx, expr: &Expr) -> Result<(), Error> {
    // Only block-like, tail-less expressions land here (parser-enforced); they
    // produce nothing on the WASM stack, so we just walk for side effects.
    match &expr.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) => codegen_unit_block_stmt(ctx, b.as_ref()),
        _ => unreachable!("only block-like exprs reach codegen_expr_stmt"),
    }
}

fn codegen_unit_block_stmt(ctx: &mut FnCtx, block: &Block) -> Result<(), Error> {
    let mark = ctx.locals.len();
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => codegen_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => codegen_assign_stmt(ctx, assign)?,
            Stmt::Expr(inner) => codegen_expr_stmt(ctx, inner)?,
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    // T4: drop in-scope Drop-typed bindings in reverse declaration order
    // before truncating the locals stack.
    emit_drops_for_locals_range(ctx, mark, ctx.locals.len())?;
    ctx.locals.truncate(mark);
    Ok(())
}

// T4: walk locals[from..to] in reverse, emit a `drop(&mut binding)`
// call for every binding whose type implements Drop. Bindings must
// already be addressed (the Drop pre-pass marked them). T4.6: skip
// any binding whose root path borrowck observed as moved — its
// destructor runs at the new owner's scope end instead.
fn emit_drops_for_locals_range(ctx: &mut FnCtx, from: usize, to: usize) -> Result<(), Error> {
    let mut i = to;
    while i > from {
        i -= 1;
        let rt = rtype_clone(&ctx.locals[i].rtype);
        if !crate::typeck::is_drop(&rt, ctx.traits) {
            continue;
        }
        // Drop requires `&mut binding` — only addressed bindings can be
        // dropped this way. Drop params aren't yet auto-addressed so
        // they're silently skipped here (a known limitation).
        if !matches!(&ctx.locals[i].storage, Storage::Memory { .. }) {
            continue;
        }
        match binding_move_status(&ctx.moved_places, &ctx.locals[i].name) {
            Some(crate::typeck::MoveStatus::Moved) => continue,
            Some(crate::typeck::MoveStatus::MaybeMoved) => {
                // Flagged drop: `if flag { drop }; end`. The flag was
                // initialized to 1 at decl, cleared to 0 at every move
                // site walked through this path, so it correctly
                // reflects whether the storage still owns its value.
                let flag_idx = lookup_drop_flag(&ctx.drop_flags, &ctx.locals[i].name)
                    .expect("MaybeMoved binding must have an allocated drop flag");
                ctx.instructions.push(wasm::Instruction::LocalGet(flag_idx));
                ctx.instructions.push(wasm::Instruction::If(wasm::BlockType::Empty));
                emit_drop_call_for_local(ctx, i, &rt)?;
                ctx.instructions.push(wasm::Instruction::End);
            }
            None => {
                emit_drop_call_for_local(ctx, i, &rt)?;
            }
        }
    }
    Ok(())
}

// T4.6: a binding is considered moved (and so should be skipped at
// scope-end drop time) when borrowck recorded a whole-binding move —
// i.e., a single-segment path equal to the binding's name. Partial moves
// (length > 1) leave the parent partially valid; for non-Drop types
// codegen still emits no drop, and for Drop types borrowck rejects the
// partial move outright.
// Returns the recorded move-status for `name` if it shows up as a
// single-segment whole-binding entry, or None if it's `Init`. This is
// what `emit_drops_for_locals_range` consults to choose between an
// unconditional drop, a skipped drop, or (later) a flagged drop.
fn binding_move_status(
    moved_places: &Vec<crate::typeck::MovedPlace>,
    name: &str,
) -> Option<crate::typeck::MoveStatus> {
    let mut i = 0;
    while i < moved_places.len() {
        if moved_places[i].place.len() == 1 && moved_places[i].place[0] == name {
            return Some(moved_places[i].status.clone());
        }
        i += 1;
    }
    None
}

fn emit_drop_call_for_local(
    ctx: &mut FnCtx,
    idx: usize,
    rtype: &RType,
) -> Result<(), Error> {
    let drop_path = crate::typeck::drop_trait_path();
    let resolution = crate::typeck::solve_impl(&drop_path, rtype, ctx.traits, 0)
        .expect("typeck verified Drop impl exists");
    let cand = crate::typeck::find_trait_impl_method(
        ctx.funcs,
        resolution.impl_idx,
        "drop",
    )
    .expect("Drop impl provides drop method");
    let callee_idx = match cand {
        crate::typeck::MethodCandidate::Direct(i) => ctx.funcs.entries[i].idx,
        crate::typeck::MethodCandidate::Template(i) => {
            let tmpl = &ctx.funcs.templates[i];
            let mut concrete: Vec<RType> = Vec::new();
            let mut k = 0;
            while k < tmpl.type_params.len() {
                let name = &tmpl.type_params[k];
                let mut found: Option<RType> = None;
                let mut j = 0;
                while j < resolution.subst.len() {
                    if resolution.subst[j].0 == *name {
                        found = Some(rtype_clone(&resolution.subst[j].1));
                        break;
                    }
                    j += 1;
                }
                concrete.push(found.expect("impl-param not bound by subst"));
                k += 1;
            }
            ctx.mono.intern(i, concrete)
        }
    };
    // Push `&mut binding` — the binding's address in shadow-stack memory.
    let frame_offset = match &ctx.locals[idx].storage {
        Storage::Memory { frame_offset } => *frame_offset,
        _ => unreachable!("Drop binding must be address-marked"),
    };
    ctx.instructions.push(wasm::Instruction::GlobalGet(0));
    if frame_offset != 0 {
        ctx.instructions
            .push(wasm::Instruction::I32Const(frame_offset as i32));
        ctx.instructions.push(wasm::Instruction::I32Add);
    }
    ctx.instructions.push(wasm::Instruction::Call(callee_idx));
    Ok(())
}

fn codegen_let_stmt(ctx: &mut FnCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    codegen_expr(ctx, &let_stmt.value)?;
    let value_id = let_stmt.value.id as usize;
    let value_ty = rtype_clone(
        ctx.expr_types[value_id]
            .as_ref()
            .expect("typeck recorded the let's type"),
    );
    let frame_offset_opt = ctx.let_offsets[value_id];

    match frame_offset_opt {
        Some(frame_offset) => {
            // Spilled — store flat scalars into memory at SP+frame_offset.
            store_flat_to_memory(ctx, &value_ty, BaseAddr::StackPointer, frame_offset);
            ctx.locals.push(LocalBinding {
                name: let_stmt.name.clone(),
                rtype: rtype_clone(&value_ty),
                storage: Storage::Memory { frame_offset },
            });
        }
        None => {
            // Non-spilled — pop flat scalars into freshly allocated WASM locals.
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&value_ty, ctx.structs, &mut vts);
            let flat_size = vts.len() as u32;
            let start = ctx.next_wasm_local;
            let mut k = 0;
            while k < vts.len() {
                ctx.extra_locals.push(vts[k].copy());
                ctx.next_wasm_local += 1;
                k += 1;
            }
            let mut k = 0;
            while k < flat_size {
                ctx.instructions
                    .push(wasm::Instruction::LocalSet(start + flat_size - 1 - k));
                k += 1;
            }
            ctx.locals.push(LocalBinding {
                name: let_stmt.name.clone(),
                rtype: rtype_clone(&value_ty),
                storage: Storage::Local {
                    wasm_start: start,
                    flat_size,
                },
            });
        }
    }
    // If this binding ends up MaybeMoved at scope-end, allocate its drop
    // flag now and init to 1 (the value just landed in storage).
    if needs_drop_flag(&ctx.moved_places, &let_stmt.name, &value_ty, ctx.traits) {
        let flag_idx = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.drop_flags.push((let_stmt.name.clone(), flag_idx));
        ctx.instructions.push(wasm::Instruction::I32Const(1));
        ctx.instructions.push(wasm::Instruction::LocalSet(flag_idx));
    }
    Ok(())
}

fn needs_drop_flag(
    moved_places: &Vec<crate::typeck::MovedPlace>,
    name: &str,
    rt: &RType,
    traits: &crate::typeck::TraitTable,
) -> bool {
    if !crate::typeck::is_drop(rt, traits) {
        return false;
    }
    let mut i = 0;
    while i < moved_places.len() {
        let mp = &moved_places[i];
        if mp.place.len() == 1
            && mp.place[0] == name
            && matches!(mp.status, crate::typeck::MoveStatus::MaybeMoved)
        {
            return true;
        }
        i += 1;
    }
    false
}

fn codegen_assign_stmt(ctx: &mut FnCtx, assign: &AssignStmt) -> Result<(), Error> {
    if let Some((deref_inner, fields)) = extract_deref_chain(&assign.lhs) {
        return codegen_deref_assign(ctx, deref_inner, &fields, &assign.rhs);
    }
    let chain = extract_place(&assign.lhs).expect("typeck verified LHS is a place");

    // Resolve the binding and walk through its rtype to find the chain target.
    let mut binding_idx: usize = 0;
    let mut found = false;
    let mut k = ctx.locals.len();
    while k > 0 {
        k -= 1;
        if ctx.locals[k].name == chain[0] {
            binding_idx = k;
            found = true;
            break;
        }
    }
    if !found {
        unreachable!("typeck verified the binding exists");
    }

    let root_ty = rtype_clone(&ctx.locals[binding_idx].rtype);
    let through_mut_ref = matches!(&root_ty, RType::Ref { mutable: true, .. });

    // Walk the chain to determine the byte offset and the target field's type.
    // For root types that are `&mut Struct`, peel off the ref; field offsets
    // are relative to the pointed-at value.
    let mut current_ty = if through_mut_ref {
        match &root_ty {
            RType::Ref { inner, .. } => rtype_clone(inner),
            _ => unreachable!(),
        }
    } else {
        rtype_clone(&root_ty)
    };
    let mut chain_offset: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        let (struct_path, struct_args) = match &current_ty {
            RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_offset: u32 = 0;
        let mut found_field = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let s = byte_size_of(&fty, ctx.structs);
            if entry.fields[j].name == chain[i] {
                chain_offset += field_offset;
                current_ty = fty;
                found_field = true;
                break;
            }
            field_offset += s;
            j += 1;
        }
        if !found_field {
            unreachable!("typeck verified the field exists");
        }
        i += 1;
    }

    // Codegen RHS.
    codegen_expr(ctx, &assign.rhs)?;

    // Determine the base address for the store.
    if through_mut_ref {
        // Read the ref's pointee address and store relative to it. The ref
        // may itself be spilled (escape analysis is name-based and over-
        // approximates), so go through the helper.
        let ref_local = ref_pointee_addr_local(ctx, binding_idx);
        store_flat_to_memory(
            ctx,
            &current_ty,
            BaseAddr::WasmLocal(ref_local),
            chain_offset,
        );
    } else {
        match &ctx.locals[binding_idx].storage {
            Storage::Memory { frame_offset } => {
                let base_off = *frame_offset + chain_offset;
                store_flat_to_memory(ctx, &current_ty, BaseAddr::StackPointer, base_off);
            }
            Storage::Local { wasm_start, .. } => {
                // Non-spilled binding: walk the chain in *flat-scalar* units
                // (not bytes) to find the destination WASM local range, then
                // LocalSet each scalar.
                let flat_chain_off = flat_chain_offset(ctx, &chain, binding_idx);
                let mut vts: Vec<wasm::ValType> = Vec::new();
                flatten_rtype(&current_ty, ctx.structs, &mut vts);
                let flat_size = vts.len() as u32;
                let start = *wasm_start + flat_chain_off;
                let mut k = 0;
                while k < flat_size {
                    ctx.instructions
                        .push(wasm::Instruction::LocalSet(start + flat_size - 1 - k));
                    k += 1;
                }
            }
        }
    }
    Ok(())
}

// For a place chain rooted at locals[binding_idx], walk through fields and
// return the flat-scalar offset of the chain's tail within the binding's flat
// representation. (Flat scalars, not bytes — for WASM-local storage.)
fn flat_chain_offset(ctx: &FnCtx, chain: &Vec<String>, binding_idx: usize) -> u32 {
    let mut current_ty = rtype_clone(&ctx.locals[binding_idx].rtype);
    let mut flat_off: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        let (struct_path, struct_args) = match &current_ty {
            RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_flat_off: u32 = 0;
        let mut found = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&fty, ctx.structs, &mut vts);
            let s = vts.len() as u32;
            if entry.fields[j].name == chain[i] {
                flat_off += field_flat_off;
                current_ty = fty;
                found = true;
                break;
            }
            field_flat_off += s;
            j += 1;
        }
        if !found {
            unreachable!("typeck verified the field exists");
        }
        i += 1;
    }
    flat_off
}

// Returns (deref_target, fields) if expr is `*E` or `(*E).f.g.h`.
fn extract_deref_chain<'a>(expr: &'a Expr) -> Option<(&'a Expr, Vec<String>)> {
    let mut fields: Vec<String> = Vec::new();
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Deref(inner) => {
                let mut reversed: Vec<String> = Vec::new();
                let mut i = fields.len();
                while i > 0 {
                    i -= 1;
                    reversed.push(fields[i].clone());
                }
                return Some((inner.as_ref(), reversed));
            }
            ExprKind::FieldAccess(fa) => {
                fields.push(fa.field.clone());
                current = &fa.base;
            }
            _ => return None,
        }
    }
}

fn codegen_deref_assign(
    ctx: &mut FnCtx,
    deref_inner: &Expr,
    fields: &Vec<String>,
    rhs: &Expr,
) -> Result<(), Error> {
    // Compute the address: codegen the deref-inner (pushes i32) and stash.
    let inner_ty = codegen_expr(ctx, deref_inner)?;
    let pointee = match &inner_ty {
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => rtype_clone(inner),
        _ => unreachable!("typeck verified deref target is a ref/raw-ptr"),
    };
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions
        .push(wasm::Instruction::LocalSet(addr_local));

    // Walk the field chain to compute the byte offset within the pointee and
    // the target field's type.
    let mut current_ty = pointee;
    let mut chain_byte_offset: u32 = 0;
    let mut i = 0;
    while i < fields.len() {
        let (struct_path, struct_args) = match &current_ty {
            RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_off: u32 = 0;
        let mut found = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let s = byte_size_of(&fty, ctx.structs);
            if entry.fields[j].name == fields[i] {
                chain_byte_offset += field_off;
                current_ty = fty;
                found = true;
                break;
            }
            field_off += s;
            j += 1;
        }
        if !found {
            unreachable!("typeck verified the field exists");
        }
        i += 1;
    }

    // Codegen RHS — pushes flat scalars matching current_ty.
    codegen_expr(ctx, rhs)?;

    // Per-leaf store: stash flat scalars, then push address+value+store.
    store_flat_to_memory(
        ctx,
        &current_ty,
        BaseAddr::WasmLocal(addr_local),
        chain_byte_offset,
    );
    Ok(())
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

fn codegen_expr(ctx: &mut FnCtx, expr: &Expr) -> Result<RType, Error> {
    match &expr.kind {
        ExprKind::IntLit(n) => {
            let ty = rtype_clone(
                ctx.expr_types[expr.id as usize]
                    .as_ref()
                    .expect("typeck recorded this literal's type"),
            );
            emit_int_lit(ctx, &ty, *n);
            Ok(ty)
        }
        ExprKind::Var(name) => codegen_var(ctx, name, expr.id),
        ExprKind::Call(call) => codegen_call(ctx, call, expr.id),
        ExprKind::StructLit(lit) => codegen_struct_lit(ctx, lit, expr.id),
        ExprKind::FieldAccess(fa) => codegen_field_access(ctx, fa),
        ExprKind::Borrow { inner, mutable } => codegen_borrow(ctx, inner, *mutable),
        ExprKind::Cast { inner, ty: _ } => {
            let src_ty = codegen_expr(ctx, inner)?;
            // The cast's resolved target type was recorded by typeck on
            // this Cast expr's NodeId — using it directly avoids a
            // re-resolution that would need its own use-scope wiring.
            let target = rtype_clone(
                ctx.expr_types[expr.id as usize]
                    .as_ref()
                    .expect("typeck recorded the cast's target type"),
            );
            // Apply the monomorphization env in case the cast target
            // contains a `Param` (e.g. inside a generic body).
            let target = substitute_rtype(&target, &ctx.env);
            // T5: integer-to-integer casts may need wasm conversion ops.
            // i32-flatten ↔ i64 transitions emit wrap_i64 / extend_i32_*.
            // Same-flatten kinds (e.g. u8 ↔ i32) are no-ops since pocket-
            // rust stores all ≤32-bit integers in a wasm i32. Refs/raw
            // pointers are also i32 → no-op for those.
            if let (RType::Int(src_k), RType::Int(tgt_k)) = (&src_ty, &target) {
                emit_int_to_int_cast(ctx, src_k, tgt_k);
            }
            Ok(target)
        }
        ExprKind::Deref(inner) => codegen_deref(ctx, inner),
        ExprKind::Unsafe(block) => codegen_block_expr(ctx, block.as_ref()),
        ExprKind::Block(block) => codegen_block_expr(ctx, block.as_ref()),
        ExprKind::MethodCall(mc) => codegen_method_call(ctx, mc, expr.id),
        ExprKind::BoolLit(b) => {
            ctx.instructions.push(wasm::Instruction::I32Const(if *b { 1 } else { 0 }));
            Ok(RType::Bool)
        }
        ExprKind::If(if_expr) => codegen_if_expr(ctx, if_expr, expr.id),
    }
}

fn codegen_if_expr(
    ctx: &mut FnCtx,
    if_expr: &crate::ast::IfExpr,
    if_node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    // Evaluate the condition (an i32 0/1) onto the stack.
    let _ = codegen_expr(ctx, &if_expr.cond)?;
    let result_ty = rtype_clone(
        ctx.expr_types[if_node_id as usize]
            .as_ref()
            .expect("typeck recorded the if's type"),
    );
    let mut flat: Vec<wasm::ValType> = Vec::new();
    crate::typeck::flatten_rtype(&result_ty, ctx.structs, &mut flat);
    let bt = match flat.len() {
        0 => wasm::BlockType::Empty,
        1 => wasm::BlockType::Single(val_type_copy(&flat[0])),
        _ => {
            // Multi-value `if` — register a FuncType (no params, these
            // results) and refer to it by index. We dedupe against
            // `ctx.pending_types`, then return base + idx so the typeidx
            // is correct after pending types are appended to
            // `wasm_mod.types` at function-emit-end.
            let ft = wasm::FuncType {
                params: Vec::new(),
                results: copy_val_type_vec(&flat),
            };
            let mut found: Option<u32> = None;
            let mut k = 0;
            while k < ctx.pending_types.len() {
                if func_type_eq(&ctx.pending_types[k], &ft) {
                    found = Some(ctx.pending_types_base + k as u32);
                    break;
                }
                k += 1;
            }
            let idx = match found {
                Some(i) => i,
                None => {
                    let i = ctx.pending_types_base + ctx.pending_types.len() as u32;
                    ctx.pending_types.push(ft);
                    i
                }
            };
            wasm::BlockType::TypeIdx(idx)
        }
    };
    ctx.instructions.push(wasm::Instruction::If(bt));
    let _ = codegen_block_expr(ctx, if_expr.then_block.as_ref())?;
    ctx.instructions.push(wasm::Instruction::Else);
    let _ = codegen_block_expr(ctx, if_expr.else_block.as_ref())?;
    ctx.instructions.push(wasm::Instruction::End);
    Ok(result_ty)
}

fn val_type_copy(vt: &wasm::ValType) -> wasm::ValType {
    match vt {
        wasm::ValType::I32 => wasm::ValType::I32,
        wasm::ValType::I64 => wasm::ValType::I64,
    }
}

fn copy_val_type_vec(v: &Vec<wasm::ValType>) -> Vec<wasm::ValType> {
    let mut out: Vec<wasm::ValType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(val_type_copy(&v[i]));
        i += 1;
    }
    out
}

fn func_type_eq(a: &wasm::FuncType, b: &wasm::FuncType) -> bool {
    if a.params.len() != b.params.len() || a.results.len() != b.results.len() {
        return false;
    }
    let mut i = 0;
    while i < a.params.len() {
        if !val_type_struct_eq(&a.params[i], &b.params[i]) {
            return false;
        }
        i += 1;
    }
    let mut i = 0;
    while i < a.results.len() {
        if !val_type_struct_eq(&a.results[i], &b.results[i]) {
            return false;
        }
        i += 1;
    }
    true
}

fn val_type_struct_eq(a: &wasm::ValType, b: &wasm::ValType) -> bool {
    matches!(
        (a, b),
        (wasm::ValType::I32, wasm::ValType::I32) | (wasm::ValType::I64, wasm::ValType::I64)
    )
}

fn codegen_method_call(
    ctx: &mut FnCtx,
    mc: &MethodCall,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let res_idx = node_id as usize;
    // Trait-dispatched calls take a separate path: solve the impl at mono
    // time, find the right impl method, and emit a direct call to it.
    let has_trait_dispatch = ctx.method_resolutions[res_idx]
        .as_ref()
        .expect("typeck registered this method call")
        .trait_dispatch
        .is_some();
    if has_trait_dispatch {
        return codegen_trait_method_call(ctx, mc, node_id);
    }
    // Match on a copy of the resolution shape to avoid borrowing ctx through
    // the resolutions vec across our subsequent codegen mutations.
    let recv_adjust_local = match &ctx.method_resolutions[res_idx]
        .as_ref()
        .expect("typeck registered this method call")
        .recv_adjust
    {
        ReceiverAdjust::Move => RecvAdjustLocal::Move,
        ReceiverAdjust::BorrowImm => RecvAdjustLocal::BorrowImm,
        ReceiverAdjust::BorrowMut => RecvAdjustLocal::BorrowMut,
        ReceiverAdjust::ByRef => RecvAdjustLocal::ByRef,
    };
    // Determine the wasm idx and return type. For non-template methods, use
    // the recorded callee_idx directly. For template methods, substitute the
    // resolution's type_args under our env, intern via MonoState, and compute
    // the return type from the template's signature.
    let template_idx_opt = ctx.method_resolutions[res_idx].as_ref().unwrap().template_idx;
    let (callee_idx, return_rt) = if let Some(template_idx) = template_idx_opt {
        let raw_args =
            rtype_vec_clone(&ctx.method_resolutions[res_idx].as_ref().unwrap().type_args);
        let concrete = subst_vec(&raw_args, &ctx.env);
        let return_rt = {
            let tmpl = &ctx.funcs.templates[template_idx];
            let tmpl_env = build_env(&tmpl.type_params, &concrete);
            match &tmpl.return_type {
                Some(rt) => substitute_rtype(rt, &tmpl_env),
                None => unreachable!("typeck rejects unit methods used as values"),
            }
        };
        let idx = ctx.mono.intern(template_idx, concrete);
        (idx, return_rt)
    } else {
        let callee_idx = ctx.method_resolutions[res_idx].as_ref().unwrap().callee_idx;
        let return_rt = {
            let entry = &ctx.funcs.entries[callee_idx_to_table_idx(ctx, callee_idx)];
            match &entry.return_type {
                Some(rt) => rtype_clone(rt),
                None => unreachable!("typeck rejects unit methods used as values"),
            }
        };
        (callee_idx, return_rt)
    };
    // Codegen receiver.
    match recv_adjust_local {
        RecvAdjustLocal::Move => {
            codegen_expr(ctx, &mc.receiver)?;
        }
        RecvAdjustLocal::BorrowImm => {
            codegen_borrow(ctx, &mc.receiver, false)?;
        }
        RecvAdjustLocal::BorrowMut => {
            codegen_borrow(ctx, &mc.receiver, true)?;
        }
        RecvAdjustLocal::ByRef => {
            codegen_expr(ctx, &mc.receiver)?;
        }
    }
    // Codegen remaining args.
    let mut i = 0;
    while i < mc.args.len() {
        codegen_expr(ctx, &mc.args[i])?;
        i += 1;
    }
    ctx.instructions.push(wasm::Instruction::Call(callee_idx));
    Ok(return_rt)
}

// Trait-dispatched method call: substitute the recorded recv type
// against the mono env, run `solve_impl` to find the impl row, look up
// the method by name, then emit a regular call to its (possibly
// monomorphized) wasm idx.
fn codegen_trait_method_call(
    ctx: &mut FnCtx,
    mc: &MethodCall,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let res_idx = node_id as usize;
    let td = ctx.method_resolutions[res_idx]
        .as_ref()
        .unwrap()
        .trait_dispatch
        .as_ref()
        .map(|t| crate::typeck::TraitDispatch {
            trait_path: clone_path(&t.trait_path),
            method_name: t.method_name.clone(),
            recv_type: rtype_clone(&t.recv_type),
        })
        .unwrap();
    // Already substituted at the time of mono cloning, but still need to
    // peel any `Ref` wrapper if the recv type was symbolic ref.
    let concrete_recv = match &td.recv_type {
        RType::Ref { inner, .. } => rtype_clone(inner),
        other => rtype_clone(other),
    };
    let resolution = match crate::typeck::solve_impl(&td.trait_path, &concrete_recv, ctx.traits, 0)
    {
        Some(r) => r,
        None => unreachable!(
            "no impl of `{}` for `{}` at mono time — typeck should have caught",
            crate::typeck::rtype_to_string(&concrete_recv),
            crate::typeck::rtype_to_string(&concrete_recv)
        ),
    };
    let cand = match crate::typeck::find_trait_impl_method(
        ctx.funcs,
        resolution.impl_idx,
        &td.method_name,
    ) {
        Some(c) => c,
        None => unreachable!("trait impl row exists but no method by that name"),
    };
    let (callee_idx, return_rt) = match cand {
        crate::typeck::MethodCandidate::Direct(i) => {
            let entry = &ctx.funcs.entries[i];
            let ret = match &entry.return_type {
                Some(rt) => rtype_clone(rt),
                None => unreachable!(),
            };
            (entry.idx, ret)
        }
        crate::typeck::MethodCandidate::Template(i) => {
            let tmpl = &ctx.funcs.templates[i];
            // T2.5b: a template's type_params are impl-level first, then
            // method-level. Impl-level slots are bound by `solve_impl`
            // (`resolution.subst`); method-level slots come from the
            // MethodResolution's recorded `type_args`, substituted
            // through this monomorphization's outer env.
            let impl_param_count = tmpl.impl_type_param_count;
            let mut concrete: Vec<RType> = Vec::new();
            let mut k = 0;
            while k < impl_param_count {
                let name = &tmpl.type_params[k];
                let mut found: Option<RType> = None;
                let mut j = 0;
                while j < resolution.subst.len() {
                    if resolution.subst[j].0 == *name {
                        found = Some(rtype_clone(&resolution.subst[j].1));
                        break;
                    }
                    j += 1;
                }
                concrete.push(found.expect("impl-param not bound by subst"));
                k += 1;
            }
            let method_param_count = tmpl.type_params.len() - impl_param_count;
            let recorded_type_args =
                rtype_vec_clone(&ctx.method_resolutions[res_idx].as_ref().unwrap().type_args);
            if recorded_type_args.len() != method_param_count {
                unreachable!(
                    "type_args length {} doesn't match method-level param count {}",
                    recorded_type_args.len(),
                    method_param_count
                );
            }
            let mut k = 0;
            while k < method_param_count {
                concrete.push(substitute_rtype(&recorded_type_args[k], &ctx.env));
                k += 1;
            }
            let tmpl_env = build_env(&tmpl.type_params, &concrete);
            let return_rt = match &tmpl.return_type {
                Some(rt) => substitute_rtype(rt, &tmpl_env),
                None => unreachable!(),
            };
            let idx = ctx.mono.intern(i, concrete);
            (idx, return_rt)
        }
    };
    // Codegen receiver per the recorded recv_adjust (derived from the
    // trait method's declared receiver shape during typeck).
    let recv_adjust = copy_recv_adjust(
        &ctx.method_resolutions[res_idx]
            .as_ref()
            .unwrap()
            .recv_adjust,
    );
    match recv_adjust {
        ReceiverAdjust::Move => {
            codegen_expr(ctx, &mc.receiver)?;
        }
        ReceiverAdjust::BorrowImm => {
            codegen_borrow(ctx, &mc.receiver, false)?;
        }
        ReceiverAdjust::BorrowMut => {
            codegen_borrow(ctx, &mc.receiver, true)?;
        }
        ReceiverAdjust::ByRef => {
            codegen_expr(ctx, &mc.receiver)?;
        }
    }
    let mut i = 0;
    while i < mc.args.len() {
        codegen_expr(ctx, &mc.args[i])?;
        i += 1;
    }
    ctx.instructions.push(wasm::Instruction::Call(callee_idx));
    Ok(return_rt)
}

enum RecvAdjustLocal {
    Move,
    BorrowImm,
    BorrowMut,
    ByRef,
}

// Map a WASM function index back to its FuncTable entry index. They're
// allocated in lockstep, so equal numerically.
fn callee_idx_to_table_idx(ctx: &FnCtx, callee_idx: u32) -> usize {
    let mut i = 0;
    while i < ctx.funcs.entries.len() {
        if ctx.funcs.entries[i].idx == callee_idx {
            return i;
        }
        i += 1;
    }
    unreachable!("callee_idx must correspond to a registered function");
}

// Emits wasm conversion ops for an int-to-int `as` cast. Pocket-rust's
// storage classes: ≤32-bit integers (and usize/isize on wasm32) flatten
// to wasm `i32`; 64-bit integers flatten to `i64`; 128-bit integers
// flatten to two `i64`s (low half on top). Same-class transitions are
// no-ops. For widening, the source's signedness drives whether the
// high bits come from sign or zero extension — matching Rust's `as`
// semantics where `-1i32 as u128` is all-bits-set.
fn emit_int_to_int_cast(ctx: &mut FnCtx, src: &IntKind, tgt: &IntKind) {
    let src_class = int_kind_class(src);
    let tgt_class = int_kind_class(tgt);
    match (src_class, tgt_class) {
        (IntClass::Wide64, IntClass::Narrow32) => {
            ctx.instructions.push(wasm::Instruction::I32WrapI64);
        }
        (IntClass::Narrow32, IntClass::Wide64) => {
            if int_kind_signed(src) {
                ctx.instructions.push(wasm::Instruction::I64ExtendI32S);
            } else {
                ctx.instructions.push(wasm::Instruction::I64ExtendI32U);
            }
        }
        (IntClass::Narrow32, IntClass::Wide128) => {
            // Widen i32 → i64 first, then synthesize the 128-bit high half.
            if int_kind_signed(src) {
                ctx.instructions.push(wasm::Instruction::I64ExtendI32S);
            } else {
                ctx.instructions.push(wasm::Instruction::I64ExtendI32U);
            }
            emit_i64_to_128_high_half(ctx, int_kind_signed(src));
        }
        (IntClass::Wide64, IntClass::Wide128) => {
            emit_i64_to_128_high_half(ctx, int_kind_signed(src));
        }
        (IntClass::Wide128, IntClass::Wide64) => {
            // Drop high half; low half (i64) is left on top.
            ctx.instructions.push(wasm::Instruction::Drop);
        }
        (IntClass::Wide128, IntClass::Narrow32) => {
            ctx.instructions.push(wasm::Instruction::Drop);
            ctx.instructions.push(wasm::Instruction::I32WrapI64);
        }
        // Same class — no wasm op needed.
        _ => {}
    }
}

// Stack: [low: i64] -> [low: i64, high: i64]. For unsigned source, the
// high half is just zero. For signed source, we duplicate `low` via a
// fresh i64 local and emit `low >> 63` (arithmetic) — propagating the
// sign bit across all 64 bits.
fn emit_i64_to_128_high_half(ctx: &mut FnCtx, signed: bool) {
    if !signed {
        ctx.instructions.push(wasm::Instruction::I64Const(0));
        return;
    }
    let tmp = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I64);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalSet(tmp));
    ctx.instructions.push(wasm::Instruction::LocalGet(tmp));
    ctx.instructions.push(wasm::Instruction::LocalGet(tmp));
    ctx.instructions.push(wasm::Instruction::I64Const(63));
    ctx.instructions.push(wasm::Instruction::I64ShrS);
}

#[derive(PartialEq, Eq)]
enum IntClass {
    Narrow32, // u8/i8/u16/i16/u32/i32/usize/isize
    Wide64,   // u64/i64
    Wide128,  // u128/i128
}

fn int_kind_class(k: &IntKind) -> IntClass {
    match k {
        IntKind::U64 | IntKind::I64 => IntClass::Wide64,
        IntKind::U128 | IntKind::I128 => IntClass::Wide128,
        _ => IntClass::Narrow32,
    }
}

fn int_kind_signed(k: &IntKind) -> bool {
    matches!(
        k,
        IntKind::I8
            | IntKind::I16
            | IntKind::I32
            | IntKind::I64
            | IntKind::I128
            | IntKind::Isize
    )
}

// T5: every user-source integer literal is desugared to
// `<T as Num>::from_i64(literal_i64)`. Codegen emits an `i64.const`
// (the only place a primitive integer constant is emitted now) and
// calls the appropriate impl method, monomorphizing when needed.
fn emit_int_lit(ctx: &mut FnCtx, ty: &RType, value: u64) {
    // Solve `<ty as Num>::from_i64`.
    let num_path = vec!["std".to_string(), "ops".to_string(), "Num".to_string()];
    let resolution = crate::typeck::solve_impl(&num_path, ty, ctx.traits, 0)
        .expect("Num impl exists for every primitive integer kind in stdlib");
    let cand = crate::typeck::find_trait_impl_method(
        ctx.funcs,
        resolution.impl_idx,
        "from_i64",
    )
    .expect("Num impl provides from_i64");
    let callee_idx = match cand {
        crate::typeck::MethodCandidate::Direct(i) => ctx.funcs.entries[i].idx,
        crate::typeck::MethodCandidate::Template(i) => {
            // Stdlib's Num impls aren't generic on themselves, so the
            // template path shouldn't fire today — but support it for
            // future user impls (e.g. `impl<T: Num> Num for Wrap<T>`).
            let tmpl = &ctx.funcs.templates[i];
            let mut concrete: Vec<RType> = Vec::new();
            let mut k = 0;
            while k < tmpl.type_params.len() {
                let name = &tmpl.type_params[k];
                let mut found: Option<RType> = None;
                let mut j = 0;
                while j < resolution.subst.len() {
                    if resolution.subst[j].0 == *name {
                        found = Some(rtype_clone(&resolution.subst[j].1));
                        break;
                    }
                    j += 1;
                }
                concrete.push(found.expect("impl-param not bound by subst"));
                k += 1;
            }
            ctx.mono.intern(i, concrete)
        }
    };
    // Push i64.const VALUE — the only direct integer-const emit; not
    // recursively desugared because it's the argument to from_i64.
    ctx.instructions
        .push(wasm::Instruction::I64Const(value as i64));
    ctx.instructions.push(wasm::Instruction::Call(callee_idx));
}

fn codegen_block_expr(ctx: &mut FnCtx, block: &Block) -> Result<RType, Error> {
    let mark = ctx.locals.len();
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => codegen_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => codegen_assign_stmt(ctx, assign)?,
            Stmt::Expr(expr) => codegen_expr_stmt(ctx, expr)?,
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    let result_ty = match &block.tail {
        Some(expr) => codegen_expr(ctx, expr)?,
        None => unreachable!("typeck rejects block expressions without a tail"),
    };
    // T4.5: drop in-scope Drop-typed bindings before yielding the tail
    // value. Save the tail to fresh locals, emit drops, reload.
    let n_after = ctx.locals.len();
    let mut tail_flat: Vec<wasm::ValType> = Vec::new();
    flatten_rtype(&result_ty, ctx.structs, &mut tail_flat);
    if !tail_flat.is_empty() {
        let save_start = ctx.next_wasm_local;
        let mut i = 0;
        while i < tail_flat.len() {
            ctx.extra_locals.push(tail_flat[i].copy());
            ctx.next_wasm_local += 1;
            i += 1;
        }
        let mut k = tail_flat.len();
        while k > 0 {
            k -= 1;
            ctx.instructions
                .push(wasm::Instruction::LocalSet(save_start + k as u32));
        }
        emit_drops_for_locals_range(ctx, mark, n_after)?;
        let mut k = 0;
        while k < tail_flat.len() {
            ctx.instructions
                .push(wasm::Instruction::LocalGet(save_start + k as u32));
            k += 1;
        }
    } else {
        emit_drops_for_locals_range(ctx, mark, n_after)?;
    }
    ctx.locals.truncate(mark);
    Ok(result_ty)
}

fn codegen_var(ctx: &mut FnCtx, name: &str, node_id: crate::ast::NodeId) -> Result<RType, Error> {
    let mut i = ctx.locals.len();
    while i > 0 {
        i -= 1;
        if ctx.locals[i].name == *name {
            let rt = rtype_clone(&ctx.locals[i].rtype);
            match &ctx.locals[i].storage {
                Storage::Local { wasm_start, flat_size } => {
                    let start = *wasm_start;
                    let n = *flat_size;
                    let mut k = 0;
                    while k < n {
                        ctx.instructions
                            .push(wasm::Instruction::LocalGet(start + k));
                        k += 1;
                    }
                }
                Storage::Memory { frame_offset } => {
                    load_flat_from_memory(ctx, &rt, BaseAddr::StackPointer, *frame_offset);
                }
            }
            // If this read is recorded as a whole-binding move site for
            // a flagged binding, clear the flag so the scope-end drop is
            // skipped on this path.
            if is_move_site(&ctx.move_sites, node_id, name) {
                if let Some(flag_idx) = lookup_drop_flag(&ctx.drop_flags, name) {
                    ctx.instructions.push(wasm::Instruction::I32Const(0));
                    ctx.instructions.push(wasm::Instruction::LocalSet(flag_idx));
                }
            }
            return Ok(rt);
        }
    }
    unreachable!("typeck verified the variable exists");
}

fn is_move_site(
    move_sites: &Vec<(crate::ast::NodeId, String)>,
    node_id: crate::ast::NodeId,
    name: &str,
) -> bool {
    let mut i = 0;
    while i < move_sites.len() {
        if move_sites[i].0 == node_id && move_sites[i].1 == name {
            return true;
        }
        i += 1;
    }
    false
}

fn lookup_drop_flag(drop_flags: &Vec<(String, u32)>, name: &str) -> Option<u32> {
    let mut i = 0;
    while i < drop_flags.len() {
        if drop_flags[i].0 == name {
            return Some(drop_flags[i].1);
        }
        i += 1;
    }
    None
}

fn codegen_call(
    ctx: &mut FnCtx,
    call: &Call,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let res_idx = node_id as usize;
    let (func_idx, return_rt) = match ctx.call_resolutions[res_idx]
        .as_ref()
        .expect("typeck registered this call")
    {
        CallResolution::Direct(idx) => {
            let entry = &ctx.funcs.entries[*idx];
            let rt = match &entry.return_type {
                Some(rt) => rtype_clone(rt),
                None => unreachable!("typeck rejects unit functions used as values"),
            };
            (entry.idx, rt)
        }
        CallResolution::Generic { template_idx, type_args } => {
            // Substitute the type_args under the current monomorphization env
            // (in case the calling function is itself a monomorphic instance
            // of a generic that called another generic with `T` flowing
            // through). The substituted args are concrete.
            let concrete = subst_vec(type_args, &ctx.env);
            let template_idx_copy = *template_idx;
            // Determine the callee's return type by substituting under the
            // template's own type-param env.
            let return_rt = {
                let tmpl: &GenericTemplate = &ctx.funcs.templates[template_idx_copy];
                let tmpl_env = build_env(&tmpl.type_params, &concrete);
                match &tmpl.return_type {
                    Some(rt) => substitute_rtype(rt, &tmpl_env),
                    None => unreachable!("typeck rejects unit functions used as values"),
                }
            };
            let idx = ctx.mono.intern(template_idx_copy, concrete);
            (idx, return_rt)
        }
    };
    let mut i = 0;
    while i < call.args.len() {
        codegen_expr(ctx, &call.args[i])?;
        i += 1;
    }
    ctx.instructions.push(wasm::Instruction::Call(func_idx));
    Ok(return_rt)
}

fn codegen_struct_lit(
    ctx: &mut FnCtx,
    lit: &StructLit,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    // Read the resolved struct type recorded by typeck at this NodeId.
    // For generic structs, this carries the concrete type_args needed for
    // layout. Substitute under our env in case those args themselves reference
    // outer Param entries (mono of mono).
    let recorded_ty = rtype_clone(
        ctx.expr_types[node_id as usize]
            .as_ref()
            .expect("typeck recorded this struct lit's type"),
    );
    let recorded_ty = substitute_rtype(&recorded_ty, &ctx.env);
    let (full, struct_args) = match &recorded_ty {
        RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
        _ => unreachable!("expr_types entry for a struct literal must be a Struct"),
    };

    // Field layouts: declaration-order (name, flat_offset, valtypes), with
    // field types substituted via the struct's type-arg env.
    struct FieldLayout {
        name: String,
        flat_offset: u32,
        valtypes: Vec<wasm::ValType>,
    }
    let layouts: Vec<FieldLayout> = {
        let entry = struct_lookup(ctx.structs, &full).expect("typeck resolved this struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut out: Vec<FieldLayout> = Vec::new();
        let mut flat_off: u32 = 0;
        let mut i = 0;
        while i < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[i].ty, &env);
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&fty, ctx.structs, &mut vts);
            let s = vts.len() as u32;
            out.push(FieldLayout {
                name: entry.fields[i].name.clone(),
                flat_offset: flat_off,
                valtypes: vts,
            });
            flat_off += s;
            i += 1;
        }
        out
    };
    let total_size: u32 = {
        let mut s: u32 = 0;
        let mut i = 0;
        while i < layouts.len() {
            s += layouts[i].valtypes.len() as u32;
            i += 1;
        }
        s
    };

    // Allocate a contiguous block of temp WASM locals to assemble the struct
    // in declaration order.
    let temp_start = ctx.next_wasm_local;
    let mut k = 0;
    while k < layouts.len() {
        let mut j = 0;
        while j < layouts[k].valtypes.len() {
            ctx.extra_locals.push(layouts[k].valtypes[j].copy());
            ctx.next_wasm_local += 1;
            j += 1;
        }
        k += 1;
    }

    // Walk lit fields in source order, drop each value into the right slot.
    let mut i = 0;
    while i < lit.fields.len() {
        codegen_expr(ctx, &lit.fields[i].value)?;
        let mut layout_idx = 0;
        while layout_idx < layouts.len() {
            if layouts[layout_idx].name == lit.fields[i].name {
                let size = layouts[layout_idx].valtypes.len() as u32;
                let mut k = 0;
                while k < size {
                    ctx.instructions.push(wasm::Instruction::LocalSet(
                        temp_start + layouts[layout_idx].flat_offset + size - 1 - k,
                    ));
                    k += 1;
                }
                break;
            }
            layout_idx += 1;
        }
        i += 1;
    }

    // Read back in declaration order.
    let mut i: u32 = 0;
    while i < total_size {
        ctx.instructions
            .push(wasm::Instruction::LocalGet(temp_start + i));
        i += 1;
    }

    Ok(RType::Struct {
        path: full,
        type_args: struct_args,
        lifetime_args: Vec::new(),
    })
}

fn codegen_field_access(ctx: &mut FnCtx, fa: &FieldAccess) -> Result<RType, Error> {
    // Try the place-rooted path (Var or Var.field… chain) for direct memory
    // access without producing the whole base on the stack.
    let chain = {
        let mut tmp: Vec<String> = Vec::new();
        tmp.push(fa.field.clone());
        if collect_place_chain(&fa.base, &mut tmp) {
            let mut reversed: Vec<String> = Vec::new();
            let mut i = tmp.len();
            while i > 0 {
                i -= 1;
                reversed.push(tmp[i].clone());
            }
            Some(reversed)
        } else {
            None
        }
    };
    if let Some(chain) = chain {
        return codegen_place_chain_load(ctx, &chain);
    }
    codegen_field_access_general(ctx, fa)
}

// Walk the spine of nested FieldAccess / Var nodes; if it bottoms out at a
// Var, push the root name and return true. Otherwise return false (and out is
// in an unspecified state — caller should drop it).
fn collect_place_chain(expr: &Expr, out: &mut Vec<String>) -> bool {
    match &expr.kind {
        ExprKind::Var(name) => {
            out.push(name.clone());
            true
        }
        ExprKind::FieldAccess(fa) => {
            out.push(fa.field.clone());
            collect_place_chain(&fa.base, out)
        }
        _ => false,
    }
}

fn codegen_place_chain_load(
    ctx: &mut FnCtx,
    chain: &Vec<String>,
) -> Result<RType, Error> {
    // Resolve the root binding.
    let mut binding_idx: usize = 0;
    let mut found = false;
    let mut k = ctx.locals.len();
    while k > 0 {
        k -= 1;
        if ctx.locals[k].name == chain[0] {
            binding_idx = k;
            found = true;
            break;
        }
    }
    if !found {
        unreachable!("typeck verified the binding exists");
    }
    let root_ty = rtype_clone(&ctx.locals[binding_idx].rtype);
    let through_ref = matches!(&root_ty, RType::Ref { .. });

    // Walk chain to compute byte offset + final type.
    let mut current_ty = if through_ref {
        match &root_ty {
            RType::Ref { inner, .. } => rtype_clone(inner),
            _ => unreachable!(),
        }
    } else {
        rtype_clone(&root_ty)
    };
    let mut chain_offset: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        let (struct_path, struct_args) = match &current_ty {
            RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_offset: u32 = 0;
        let mut found_field = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let s = byte_size_of(&fty, ctx.structs);
            if entry.fields[j].name == chain[i] {
                chain_offset += field_offset;
                current_ty = fty;
                found_field = true;
                break;
            }
            field_offset += s;
            j += 1;
        }
        if !found_field {
            unreachable!("typeck verified the field exists");
        }
        i += 1;
    }

    if through_ref {
        let ref_local = ref_pointee_addr_local(ctx, binding_idx);
        load_flat_from_memory(ctx, &current_ty, BaseAddr::WasmLocal(ref_local), chain_offset);
    } else {
        match &ctx.locals[binding_idx].storage {
            Storage::Memory { frame_offset } => {
                load_flat_from_memory(
                    ctx,
                    &current_ty,
                    BaseAddr::StackPointer,
                    *frame_offset + chain_offset,
                );
            }
            Storage::Local { .. } => {
                // Non-spilled value — fall back to the flat-scalar dance.
                return codegen_field_access_general_for_chain(ctx, chain, binding_idx);
            }
        }
    }
    Ok(current_ty)
}

fn codegen_field_access_general(ctx: &mut FnCtx, fa: &FieldAccess) -> Result<RType, Error> {
    // Produce the base value on the stack, then extract the desired field via
    // the stash-and-restore dance over fresh WASM locals.
    let base_type = codegen_expr(ctx, &fa.base)?;
    extract_field_from_stack(ctx, &base_type, &fa.field)
}

fn codegen_field_access_general_for_chain(
    ctx: &mut FnCtx,
    chain: &Vec<String>,
    binding_idx: usize,
) -> Result<RType, Error> {
    // Produce the binding's whole flat value on the stack, then walk the chain
    // applying extract_field_from_stack at each step.
    let start = match &ctx.locals[binding_idx].storage {
        Storage::Local { wasm_start, flat_size } => {
            let mut k = 0;
            while k < *flat_size {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(*wasm_start + k));
                k += 1;
            }
            *wasm_start
        }
        _ => unreachable!(),
    };
    let _ = start;
    let mut current_ty = rtype_clone(&ctx.locals[binding_idx].rtype);
    let mut i = 1;
    while i < chain.len() {
        current_ty = extract_field_from_stack(ctx, &current_ty, &chain[i])?;
        i += 1;
    }
    Ok(current_ty)
}

fn extract_field_from_stack(
    ctx: &mut FnCtx,
    base_type: &RType,
    field_name: &str,
) -> Result<RType, Error> {
    // Compute total flat size, field flat offset, field flat size, field type.
    let (struct_path, struct_args) = match base_type {
        RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
        RType::Ref { inner, .. } => match inner.as_ref() {
            RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
            _ => unreachable!("typeck rejects field access on non-struct"),
        },
        _ => unreachable!("typeck rejects field access on non-struct"),
    };
    let mut total_flat: u32 = 0;
    let mut field_flat_off: u32 = 0;
    let mut field_valtypes: Vec<wasm::ValType> = Vec::new();
    let mut field_ty: RType = RType::Int(IntKind::I32);
    {
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut found = false;
        let mut i = 0;
        while i < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[i].ty, &env);
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&fty, ctx.structs, &mut vts);
            let s = vts.len() as u32;
            if entry.fields[i].name == field_name {
                field_flat_off = total_flat;
                field_valtypes = vts;
                field_ty = fty;
                found = true;
            }
            total_flat += s;
            i += 1;
        }
        if !found {
            unreachable!("typeck verified field");
        }
    }
    let field_size = field_valtypes.len() as u32;
    let drop_top = total_flat - field_flat_off - field_size;

    let mut i = 0;
    while i < drop_top {
        ctx.instructions.push(wasm::Instruction::Drop);
        i += 1;
    }
    let stash_start = ctx.next_wasm_local;
    let mut k = 0;
    while k < field_valtypes.len() {
        ctx.extra_locals.push(field_valtypes[k].copy());
        ctx.next_wasm_local += 1;
        k += 1;
    }
    let mut k = 0;
    while k < field_size {
        ctx.instructions
            .push(wasm::Instruction::LocalSet(stash_start + field_size - 1 - k));
        k += 1;
    }
    let mut i = 0;
    while i < field_flat_off {
        ctx.instructions.push(wasm::Instruction::Drop);
        i += 1;
    }
    let mut k = 0;
    while k < field_size {
        ctx.instructions
            .push(wasm::Instruction::LocalGet(stash_start + k));
        k += 1;
    }
    Ok(field_ty)
}

fn codegen_borrow(ctx: &mut FnCtx, inner: &Expr, mutable: bool) -> Result<RType, Error> {
    let chain =
        extract_place(inner).expect("typeck verified the borrow operand is a place expression");
    let mut binding_idx: usize = 0;
    let mut found = false;
    let mut k = ctx.locals.len();
    while k > 0 {
        k -= 1;
        if ctx.locals[k].name == chain[0] {
            binding_idx = k;
            found = true;
            break;
        }
    }
    if !found {
        unreachable!("typeck verified the binding exists");
    }
    let root_ty = rtype_clone(&ctx.locals[binding_idx].rtype);
    // Borrowing `&r.field…` where r is a ref binding doesn't take r's address —
    // it takes the *pointee's* field address. The base is r's i32 value, not
    // SP+frame_offset. (For chain.len() == 1, falls into the SP-relative path
    // below — `&r` *does* take r's address, producing `&&T`.)
    let through_ref = matches!(&root_ty, RType::Ref { .. }) && chain.len() >= 2;

    // Walk chain to byte offset + final type.
    let mut current_ty = if through_ref {
        match &root_ty {
            RType::Ref { inner, .. } => rtype_clone(inner),
            _ => unreachable!(),
        }
    } else {
        rtype_clone(&root_ty)
    };
    let mut chain_offset: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        let (struct_path, struct_args) = match &current_ty {
            RType::Struct { path, type_args, .. } => (clone_path(path), rtype_vec_clone(type_args)),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_offset: u32 = 0;
        let mut j = 0;
        let mut found_field = false;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let s = byte_size_of(&fty, ctx.structs);
            if entry.fields[j].name == chain[i] {
                chain_offset += field_offset;
                current_ty = fty;
                found_field = true;
                break;
            }
            field_offset += s;
            j += 1;
        }
        if !found_field {
            unreachable!("typeck verified the field exists");
        }
        i += 1;
    }
    if through_ref {
        // Base address = ref's pointee address (the i32 the ref carries).
        let base_local = ref_pointee_addr_local(ctx, binding_idx);
        ctx.instructions
            .push(wasm::Instruction::LocalGet(base_local));
        if chain_offset != 0 {
            ctx.instructions
                .push(wasm::Instruction::I32Const(chain_offset as i32));
            ctx.instructions.push(wasm::Instruction::I32Add);
        }
    } else {
        // The binding must be spilled (escape analysis enforces this for any
        // binding whose address is taken).
        let frame_offset = match &ctx.locals[binding_idx].storage {
            Storage::Memory { frame_offset } => *frame_offset,
            Storage::Local { .. } => {
                unreachable!("escape analysis must have spilled this binding");
            }
        };
        ctx.instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        let total = frame_offset + chain_offset;
        if total != 0 {
            ctx.instructions
                .push(wasm::Instruction::I32Const(total as i32));
            ctx.instructions.push(wasm::Instruction::I32Add);
        }
    }
    Ok(RType::Ref {
        inner: Box::new(current_ty),
        mutable,
        // Codegen doesn't track lifetimes — `Inferred(0)` placeholder.
        lifetime: crate::typeck::LifetimeRepr::Inferred(0),
    })
}

// Returns a WASM local whose value is the i32 pointee address held by a ref
// binding. If the ref is non-spilled (just sits in a local), that's the local
// itself. If the ref is spilled (its i32 is in memory), allocate a temp local,
// load the i32 from memory once, and return the temp.
fn ref_pointee_addr_local(ctx: &mut FnCtx, binding_idx: usize) -> u32 {
    match &ctx.locals[binding_idx].storage {
        Storage::Local { wasm_start, .. } => *wasm_start,
        Storage::Memory { frame_offset } => {
            let off = *frame_offset;
            let temp = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions
                .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
            ctx.instructions
                .push(wasm::Instruction::I32Load { align: 0, offset: off });
            ctx.instructions.push(wasm::Instruction::LocalSet(temp));
            temp
        }
    }
}

fn codegen_deref(ctx: &mut FnCtx, inner: &Expr) -> Result<RType, Error> {
    // Inner produces an i32 address. Stash it, then load each leaf of the
    // pointee type from address+leaf_offset.
    let inner_ty = codegen_expr(ctx, inner)?;
    let pointee = match &inner_ty {
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => rtype_clone(inner),
        _ => unreachable!("typeck rejects deref of non-reference"),
    };
    // Stash the address in a fresh i32 local so we can reuse it across leaves.
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions
        .push(wasm::Instruction::LocalSet(addr_local));
    load_flat_from_memory(ctx, &pointee, BaseAddr::WasmLocal(addr_local), 0);
    Ok(pointee)
}

// ============================================================================
// Memory plumbing helpers
// ============================================================================

#[derive(Copy, Clone)]
enum BaseAddr {
    StackPointer,
    WasmLocal(u32),
}

fn emit_base(ctx: &mut FnCtx, base: BaseAddr) {
    match base {
        BaseAddr::StackPointer => ctx
            .instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL)),
        BaseAddr::WasmLocal(i) => ctx.instructions.push(wasm::Instruction::LocalGet(i)),
    }
}

// Pop flat scalars off the WASM stack and store them at base+offset+leaf_offset
// in memory.
fn store_flat_to_memory(ctx: &mut FnCtx, ty: &RType, base: BaseAddr, base_offset: u32) {
    let mut leaves: Vec<MemLeaf> = Vec::new();
    collect_leaves(ty, ctx.structs, 0, &mut leaves);
    if leaves.is_empty() {
        return;
    }
    // Allocate temps matching the leaves' valtypes.
    let mut temps: Vec<u32> = Vec::with_capacity(leaves.len());
    let mut i = 0;
    while i < leaves.len() {
        let idx = ctx.next_wasm_local;
        ctx.extra_locals.push(leaves[i].valtype.copy());
        ctx.next_wasm_local += 1;
        temps.push(idx);
        i += 1;
    }
    // Pop scalars: top of stack is the LAST leaf, so set in reverse.
    let mut k = leaves.len();
    while k > 0 {
        k -= 1;
        ctx.instructions.push(wasm::Instruction::LocalSet(temps[k]));
    }
    // Per-leaf: push base, push value, store.
    let mut k = 0;
    while k < leaves.len() {
        emit_base(ctx, base);
        ctx.instructions.push(wasm::Instruction::LocalGet(temps[k]));
        ctx.instructions.push(store_instr(&leaves[k], base_offset));
        k += 1;
    }
}

// Push flat scalars onto the WASM stack, loading them from base+offset+leaf_offset.
fn load_flat_from_memory(ctx: &mut FnCtx, ty: &RType, base: BaseAddr, base_offset: u32) {
    let mut leaves: Vec<MemLeaf> = Vec::new();
    collect_leaves(ty, ctx.structs, 0, &mut leaves);
    let mut k = 0;
    while k < leaves.len() {
        emit_base(ctx, base);
        ctx.instructions.push(load_instr(&leaves[k], base_offset));
        k += 1;
    }
}
