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
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut module_path: Vec<String> = Vec::new();
    push_root_name(&mut module_path, root);
    // Monomorphic instantiations get wasm idxs starting after the non-generic
    // entries' idxs (which typeck assigned 0..entries.len()).
    let mut mono = MonoState::new(funcs.entries.len() as u32);
    emit_module(wasm_mod, root, &mut module_path, structs, funcs, &mut mono)?;
    // Drain in FIFO order so wasm_mod.functions index matches the assigned
    // wasm_idx. (Each emit_monomorphic may enqueue more work — those go to the
    // end and are processed after the current batch.)
    while !mono.queue.is_empty() {
        let work = mono.queue.remove(0);
        emit_monomorphic(wasm_mod, work, structs, funcs, &mut mono)?;
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
    funcs: &'a FuncTable,
    current_module: Vec<String>,
    let_types: Vec<RType>,
    lit_types: Vec<RType>,
    struct_lit_types: Vec<RType>,
    let_offsets: Vec<Option<u32>>,
    let_idx: usize,
    lit_idx: usize,
    struct_lit_idx: usize,
    method_resolutions: Vec<MethodResolution>,
    method_idx: usize,
    call_resolutions: Vec<CallResolution>,
    call_idx: usize,
    self_target: Option<RType>,
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
        RType::Struct { path, type_args } => {
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
    let_addressed: Vec<bool>,
}

fn analyze_addresses(func: &Function, let_count: usize) -> AddressInfo {
    let mut info = AddressInfo {
        param_addressed: vec_of_false(func.params.len()),
        let_addressed: vec_of_false(let_count),
    };
    let mut stack: Vec<BindingRef> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        stack.push(BindingRef::Param(k, func.params[k].name.clone()));
        k += 1;
    }
    let mut let_idx: usize = 0;
    walk_block_addr(&func.body, &mut stack, &mut let_idx, &mut info);
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
    Let(usize, String),
}

fn binding_ref_name<'a>(b: &'a BindingRef) -> &'a str {
    match b {
        BindingRef::Param(_, n) | BindingRef::Let(_, n) => n,
    }
}

fn walk_block_addr(
    block: &Block,
    stack: &mut Vec<BindingRef>,
    let_idx: &mut usize,
    info: &mut AddressInfo,
) {
    let mark = stack.len();
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                walk_expr_addr(&let_stmt.value, stack, let_idx, info);
                let id = *let_idx;
                *let_idx += 1;
                stack.push(BindingRef::Let(id, let_stmt.name.clone()));
            }
            Stmt::Assign(assign) => {
                walk_expr_addr(&assign.lhs, stack, let_idx, info);
                walk_expr_addr(&assign.rhs, stack, let_idx, info);
            }
            Stmt::Expr(expr) => walk_expr_addr(expr, stack, let_idx, info),
        }
        i += 1;
    }
    if let Some(tail) = &block.tail {
        walk_expr_addr(tail, stack, let_idx, info);
    }
    while stack.len() > mark {
        stack.pop();
    }
}

fn walk_expr_addr(
    expr: &Expr,
    stack: &mut Vec<BindingRef>,
    let_idx: &mut usize,
    info: &mut AddressInfo,
) {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::Var(_) => {}
        ExprKind::Borrow { inner, .. } => {
            if let Some(chain) = extract_place(inner) {
                let root = &chain[0];
                let mut i = stack.len();
                while i > 0 {
                    i -= 1;
                    if binding_ref_name(&stack[i]) == root {
                        match &stack[i] {
                            BindingRef::Param(idx, _) => info.param_addressed[*idx] = true,
                            BindingRef::Let(idx, _) => info.let_addressed[*idx] = true,
                        }
                        break;
                    }
                }
            }
            walk_expr_addr(inner, stack, let_idx, info);
        }
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                walk_expr_addr(&c.args[i], stack, let_idx, info);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                walk_expr_addr(&s.fields[i].value, stack, let_idx, info);
                i += 1;
            }
        }
        ExprKind::FieldAccess(fa) => {
            walk_expr_addr(&fa.base, stack, let_idx, info);
        }
        ExprKind::Cast { inner, .. } => walk_expr_addr(inner, stack, let_idx, info),
        ExprKind::Deref(inner) => walk_expr_addr(inner, stack, let_idx, info),
        ExprKind::Unsafe(b) => walk_block_addr(b.as_ref(), stack, let_idx, info),
        ExprKind::Block(b) => walk_block_addr(b.as_ref(), stack, let_idx, info),
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
                            BindingRef::Let(idx, _) => info.let_addressed[*idx] = true,
                        }
                        break;
                    }
                }
            }
            walk_expr_addr(&mc.receiver, stack, let_idx, info);
            let mut i = 0;
            while i < mc.args.len() {
                walk_expr_addr(&mc.args[i], stack, let_idx, info);
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
                    emit_function(wasm_mod, f, path, path, None, structs, funcs, mono)?;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                emit_module(wasm_mod, m, path, structs, funcs, mono)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Impl(ib) => {
                if ib.target.segments.len() != 1 {
                    continue;
                }
                let target_name = ib.target.segments[0].name.clone();
                let mut method_prefix = clone_path(path);
                method_prefix.push(target_name);
                let mut target_path = clone_path(path);
                target_path.push(ib.target.segments[0].name.clone());
                let mut impl_param_args: Vec<RType> = Vec::new();
                let mut k = 0;
                while k < ib.type_params.len() {
                    impl_param_args.push(RType::Param(ib.type_params[k].name.clone()));
                    k += 1;
                }
                let target_rt = RType::Struct {
                    path: target_path,
                    type_args: impl_param_args,
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
                            funcs,
                            mono,
                        )?;
                    }
                    k += 1;
                }
            }
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
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let tmpl = &funcs.templates[work.template_idx];
    let env = build_env(&tmpl.type_params, &work.type_args);
    let param_types = subst_vec(&tmpl.param_types, &env);
    let return_type = tmpl.return_type.as_ref().map(|t| substitute_rtype(t, &env));
    let let_types = subst_vec(&tmpl.let_types, &env);
    let lit_types = subst_vec(&tmpl.lit_types, &env);
    let struct_lit_types = subst_vec(&tmpl.struct_lit_types, &env);
    let method_resolutions = clone_method_resolutions(&tmpl.method_resolutions, &env);
    let call_resolutions = subst_call_resolutions(&tmpl.call_resolutions, &env);
    // Determine self_target for the body: if this template is a method (its
    // path ends with method_name and the parent path is a struct), we may need
    // to pass a Self target for resolve_type. For free generic fns, None.
    // Phase 1 templates were always free fns, so this only matters for generic
    // methods (Phase 3) — handled below by checking the struct lookup.
    let self_target: Option<RType> = if tmpl.path.len() >= 2 {
        let parent_path: Vec<String> = tmpl.path[..tmpl.path.len() - 1].to_vec();
        if let Some(struct_entry) = crate::typeck::struct_lookup(structs, &parent_path) {
            // Build self target with type_args from the env (impl's type_params come first).
            let mut self_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < struct_entry.type_params.len() {
                // The first impl_type_params slots in env correspond to struct's type params.
                self_args.push(rtype_clone(&work.type_args[i]));
                i += 1;
            }
            Some(RType::Struct {
                path: parent_path,
                type_args: self_args,
            })
        } else {
            None
        }
    } else {
        None
    };
    emit_function_concrete(
        wasm_mod,
        &tmpl.func,
        &tmpl.enclosing_module,
        &tmpl.enclosing_module,
        self_target.as_ref(),
        structs,
        funcs,
        mono,
        param_types,
        return_type,
        let_types,
        lit_types,
        struct_lit_types,
        method_resolutions,
        call_resolutions,
        env,
        tmpl.type_params.clone(),
        work.wasm_idx,
        false, // monomorphic instances are never exported
    )
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

fn clone_method_resolutions(
    v: &Vec<MethodResolution>,
    env: &Vec<(String, RType)>,
) -> Vec<MethodResolution> {
    let mut out: Vec<MethodResolution> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let mut subst_args: Vec<RType> = Vec::new();
        let mut j = 0;
        while j < v[i].type_args.len() {
            subst_args.push(substitute_rtype(&v[i].type_args[j], env));
            j += 1;
        }
        out.push(MethodResolution {
            callee_idx: v[i].callee_idx,
            callee_path: clone_path(&v[i].callee_path),
            recv_adjust: copy_recv_adjust(&v[i].recv_adjust),
            ret_borrows_receiver: v[i].ret_borrows_receiver,
            template_idx: v[i].template_idx,
            type_args: subst_args,
        });
        i += 1;
    }
    out
}

fn copy_recv_adjust(r: &ReceiverAdjust) -> ReceiverAdjust {
    match r {
        ReceiverAdjust::Move => ReceiverAdjust::Move,
        ReceiverAdjust::BorrowImm => ReceiverAdjust::BorrowImm,
        ReceiverAdjust::BorrowMut => ReceiverAdjust::BorrowMut,
        ReceiverAdjust::ByRef => ReceiverAdjust::ByRef,
    }
}

fn subst_call_resolutions(
    v: &Vec<CallResolution>,
    env: &Vec<(String, RType)>,
) -> Vec<CallResolution> {
    let mut out: Vec<CallResolution> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(match &v[i] {
            CallResolution::Direct(idx) => CallResolution::Direct(*idx),
            CallResolution::Generic { template_idx, type_args } => CallResolution::Generic {
                template_idx: *template_idx,
                type_args: subst_vec(type_args, env),
            },
        });
        i += 1;
    }
    out
}

fn emit_function(
    wasm_mod: &mut wasm::Module,
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    structs: &StructTable,
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
    let let_types = rtype_vec_clone(&entry.let_types);
    let lit_types = rtype_vec_clone(&entry.lit_types);
    let struct_lit_types = rtype_vec_clone(&entry.struct_lit_types);
    let method_resolutions = clone_method_resolutions(&entry.method_resolutions, &Vec::new());
    let call_resolutions = subst_call_resolutions(&entry.call_resolutions, &Vec::new());
    let wasm_idx = entry.idx;
    let is_export = current_module.is_empty() && path_prefix.len() == current_module.len();
    emit_function_concrete(
        wasm_mod,
        func,
        current_module,
        path_prefix,
        self_target,
        structs,
        funcs,
        mono,
        param_types,
        return_type,
        let_types,
        lit_types,
        struct_lit_types,
        method_resolutions,
        call_resolutions,
        Vec::new(),
        Vec::new(),
        wasm_idx,
        is_export,
    )
}

fn emit_function_concrete(
    wasm_mod: &mut wasm::Module,
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    structs: &StructTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
    param_types: Vec<RType>,
    return_type: Option<RType>,
    let_types: Vec<RType>,
    lit_types: Vec<RType>,
    struct_lit_types: Vec<RType>,
    method_resolutions: Vec<MethodResolution>,
    call_resolutions: Vec<CallResolution>,
    env: Vec<(String, RType)>,
    type_params: Vec<String>,
    wasm_idx: u32,
    is_export: bool,
) -> Result<(), Error> {
    let _ = path_prefix;
    // Address-taken analysis: who needs to live in shadow-stack memory?
    let address_info = analyze_addresses(func, let_types.len());

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
    let mut let_offsets: Vec<Option<u32>> = Vec::with_capacity(let_types.len());
    let mut k = 0;
    while k < let_types.len() {
        if address_info.let_addressed[k] {
            let_offsets.push(Some(frame_size));
            frame_size += byte_size_of(&let_types[k], structs);
        } else {
            let_offsets.push(None);
        }
        k += 1;
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
        funcs,
        current_module: clone_path(current_module),
        let_types,
        lit_types,
        struct_lit_types,
        let_offsets,
        let_idx: 0,
        lit_idx: 0,
        struct_lit_idx: 0,
        method_resolutions,
        method_idx: 0,
        call_resolutions,
        call_idx: 0,
        self_target: self_target.map(|rt| rtype_clone(rt)),
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

    codegen_block(&mut ctx, &func.body)?;

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
        }
        i += 1;
    }
    // No tail (parser ensures it).
    ctx.locals.truncate(mark);
    Ok(())
}

fn codegen_let_stmt(ctx: &mut FnCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    // Codegen the RHS first — its own let_idx slots get consumed during the
    // recursion. Our slot is whichever index is current after that returns.
    codegen_expr(ctx, &let_stmt.value)?;
    let value_ty = rtype_clone(&ctx.let_types[ctx.let_idx]);
    let frame_offset_opt = ctx.let_offsets[ctx.let_idx];
    ctx.let_idx += 1;

    match frame_offset_opt {
        Some(frame_offset) => {
            // Spilled — store flat scalars into memory at SP+frame_offset.
            store_flat_to_memory(ctx, &value_ty, BaseAddr::StackPointer, frame_offset);
            ctx.locals.push(LocalBinding {
                name: let_stmt.name.clone(),
                rtype: value_ty,
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
                rtype: value_ty,
                storage: Storage::Local {
                    wasm_start: start,
                    flat_size,
                },
            });
        }
    }
    Ok(())
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
            RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
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
            RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
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
            RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
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
            let ty = rtype_clone(&ctx.lit_types[ctx.lit_idx]);
            ctx.lit_idx += 1;
            emit_int_lit(ctx, &ty, *n);
            Ok(ty)
        }
        ExprKind::Var(name) => codegen_var(ctx, name),
        ExprKind::Call(call) => codegen_call(ctx, call),
        ExprKind::StructLit(lit) => codegen_struct_lit(ctx, lit),
        ExprKind::FieldAccess(fa) => codegen_field_access(ctx, fa),
        ExprKind::Borrow { inner, mutable } => codegen_borrow(ctx, inner, *mutable),
        ExprKind::Cast { inner, ty } => {
            // Inner produces an i32 (refs / raw pointers are i32; cast-sourced
            // integer literals were pinned to usize by typeck → i32). The cast
            // is a type-only reinterpretation at WASM level.
            codegen_expr(ctx, inner)?;
            resolve_type(
                ty,
                &ctx.current_module,
                ctx.structs,
                ctx.self_target.as_ref(),
                &Vec::new(),
                "",
            )
            .map_err(|e| e)
        }
        ExprKind::Deref(inner) => codegen_deref(ctx, inner),
        ExprKind::Unsafe(block) => codegen_block_expr(ctx, block.as_ref()),
        ExprKind::Block(block) => codegen_block_expr(ctx, block.as_ref()),
        ExprKind::MethodCall(mc) => codegen_method_call(ctx, mc),
    }
}

fn codegen_method_call(ctx: &mut FnCtx, mc: &MethodCall) -> Result<RType, Error> {
    let res_idx = ctx.method_idx;
    ctx.method_idx += 1;
    // Match on a copy of the resolution shape to avoid borrowing ctx through
    // the resolutions vec across our subsequent codegen mutations.
    let recv_adjust_local = match &ctx.method_resolutions[res_idx].recv_adjust {
        ReceiverAdjust::Move => RecvAdjustLocal::Move,
        ReceiverAdjust::BorrowImm => RecvAdjustLocal::BorrowImm,
        ReceiverAdjust::BorrowMut => RecvAdjustLocal::BorrowMut,
        ReceiverAdjust::ByRef => RecvAdjustLocal::ByRef,
    };
    // Determine the wasm idx and return type. For non-template methods, use
    // the recorded callee_idx directly. For template methods, substitute the
    // resolution's type_args under our env, intern via MonoState, and compute
    // the return type from the template's signature.
    let template_idx_opt = ctx.method_resolutions[res_idx].template_idx;
    let (callee_idx, return_rt) = if let Some(template_idx) = template_idx_opt {
        let raw_args = rtype_vec_clone(&ctx.method_resolutions[res_idx].type_args);
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
        let callee_idx = ctx.method_resolutions[res_idx].callee_idx;
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

fn emit_int_lit(ctx: &mut FnCtx, ty: &RType, value: u64) {
    let kind = match ty {
        RType::Int(k) => k,
        _ => unreachable!("typeck assigned a non-int type to an integer literal"),
    };
    match kind {
        IntKind::U64 | IntKind::I64 => {
            ctx.instructions
                .push(wasm::Instruction::I64Const(value as i64));
        }
        IntKind::U128 | IntKind::I128 => {
            ctx.instructions
                .push(wasm::Instruction::I64Const(value as i64));
            ctx.instructions.push(wasm::Instruction::I64Const(0));
        }
        _ => {
            ctx.instructions
                .push(wasm::Instruction::I32Const(value as u32 as i32));
        }
    }
}

fn codegen_block_expr(ctx: &mut FnCtx, block: &Block) -> Result<RType, Error> {
    let mark = ctx.locals.len();
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => codegen_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => codegen_assign_stmt(ctx, assign)?,
            Stmt::Expr(expr) => codegen_expr_stmt(ctx, expr)?,
        }
        i += 1;
    }
    let result_ty = match &block.tail {
        Some(expr) => codegen_expr(ctx, expr)?,
        None => unreachable!("typeck rejects block expressions without a tail"),
    };
    ctx.locals.truncate(mark);
    Ok(result_ty)
}

fn codegen_var(ctx: &mut FnCtx, name: &str) -> Result<RType, Error> {
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
            return Ok(rt);
        }
    }
    unreachable!("typeck verified the variable exists");
}

fn codegen_call(ctx: &mut FnCtx, call: &Call) -> Result<RType, Error> {
    let res_idx = ctx.call_idx;
    ctx.call_idx += 1;
    let (func_idx, return_rt) = match &ctx.call_resolutions[res_idx] {
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

fn codegen_struct_lit(ctx: &mut FnCtx, lit: &StructLit) -> Result<RType, Error> {
    // Read the resolved struct type recorded by typeck (in source-DFS order).
    // For generic structs, this carries the concrete type_args needed for
    // layout. Substitute under our env in case those args themselves reference
    // outer Param entries (mono of mono).
    let recorded_idx = ctx.struct_lit_idx;
    ctx.struct_lit_idx += 1;
    let recorded_ty = rtype_clone(&ctx.struct_lit_types[recorded_idx]);
    let recorded_ty = substitute_rtype(&recorded_ty, &ctx.env);
    let (full, struct_args) = match &recorded_ty {
        RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
        _ => unreachable!("struct_lit_types must hold a Struct"),
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
            RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
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
        RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
        RType::Ref { inner, .. } => match inner.as_ref() {
            RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
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
            RType::Struct { path, type_args } => (clone_path(path), rtype_vec_clone(type_args)),
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
