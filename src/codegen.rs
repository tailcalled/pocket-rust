use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, MethodCall,
    Module, Path, Pattern, Stmt, StructLit,
};
use crate::span::Error;
use crate::typeck::{
    CallResolution, FuncTable, GenericTemplate, IntKind, MethodResolution, RType, ReceiverAdjust,
    StructTable, byte_size_of, flatten_rtype, func_lookup, rtype_eq, struct_lookup, substitute_rtype,
};
use crate::wasm;

// Globals seeded by `lib.rs`: index 0 is the shadow-stack pointer
// (`__sp`); index 1 is the heap top (`__heap_top`, bump-allocator
// cursor for `¤alloc`).
const SP_GLOBAL: u32 = 0;
const HEAP_GLOBAL: u32 = 1;

// Tracks monomorphic instantiations of generic templates. Maps each
// (template_idx, concrete type_args) to a wasm function index; queues new ones
// for later emission. Codegen drains the queue after the AST walk.
struct MonoState {
    queue: Vec<MonoWork>,
    map_template: Vec<usize>,
    map_args: Vec<Vec<RType>>,
    map_idx: Vec<u32>,
    next_idx: u32,
    // Per-crate string-literal pool. Each entry is a deduped UTF-8
    // payload; offsets are relative to the start of *this crate's*
    // contribution. `str_pool_base_offset` is the absolute byte
    // offset within the wasm data segment where this crate's
    // contribution starts (i.e. cumulative size of earlier crates'
    // pool contributions). `intern_str` returns absolute memory
    // addresses for codegen.
    str_pool_bytes: Vec<u8>,
    str_pool_entries: Vec<StrPoolEntry>,
    str_pool_base_offset: u32,
}

struct StrPoolEntry {
    payload: String,
    relative_offset: u32,
}

struct MonoWork {
    template_idx: usize,
    type_args: Vec<RType>,
    wasm_idx: u32,
}

impl MonoState {
    fn new(start_idx: u32, str_pool_base_offset: u32) -> Self {
        Self {
            queue: Vec::new(),
            map_template: Vec::new(),
            map_args: Vec::new(),
            map_idx: Vec::new(),
            next_idx: start_idx,
            str_pool_bytes: Vec::new(),
            str_pool_entries: Vec::new(),
            str_pool_base_offset,
        }
    }

    // Intern a string literal into this crate's pool. Returns the
    // **absolute memory address** of the entry plus its byte length —
    // i.e. `STR_POOL_BASE + cumulative-prior-pool-size + relative`.
    // Dedupes against earlier entries with the same payload so
    // repeated `"hello"` literals in this crate share a single slot.
    fn intern_str(&mut self, payload: &str) -> (u32, u32) {
        let mut i = 0;
        while i < self.str_pool_entries.len() {
            if self.str_pool_entries[i].payload == payload {
                let rel = self.str_pool_entries[i].relative_offset;
                let absolute = STR_POOL_BASE + self.str_pool_base_offset + rel;
                return (absolute, payload.as_bytes().len() as u32);
            }
            i += 1;
        }
        let bytes = payload.as_bytes();
        let relative = self.str_pool_bytes.len() as u32;
        let mut k = 0;
        while k < bytes.len() {
            self.str_pool_bytes.push(bytes[k]);
            k += 1;
        }
        self.str_pool_entries.push(StrPoolEntry {
            payload: payload.to_string(),
            relative_offset: relative,
        });
        let absolute = STR_POOL_BASE + self.str_pool_base_offset + relative;
        (absolute, bytes.len() as u32)
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
        self.map_args.push(type_args.clone());
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
        env.push((type_params[i].clone(), type_args[i].clone()));
        i += 1;
    }
    env
}

// Address at which the module's string-literal data segment starts.
// Sits in low memory just past the null-territory bytes (offset 0..7).
// The heap (`__heap_top`) is bumped past the pool at end-of-emit so
// it doesn't collide with the baked-in string data.
pub const STR_POOL_BASE: u32 = 8;

pub fn emit(
    wasm_mod: &mut wasm::Module,
    root: &Module,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut module_path: Vec<String> = Vec::new();
    push_root_name(&mut module_path, root);
    // Monomorphic instantiations get wasm idxs starting after the non-generic
    // entries' idxs (which typeck assigned 0..entries.len()).
    //
    // The string-literal pool's per-crate base offset is the
    // cumulative size of any earlier crates' contributions to the
    // single segment at STR_POOL_BASE — read from the wasm module's
    // existing data segments.
    let str_pool_base_offset: u32 = {
        let mut total: u32 = 0;
        let mut i = 0;
        while i < wasm_mod.datas.len() {
            if wasm_mod.datas[i].offset == STR_POOL_BASE {
                total = wasm_mod.datas[i].bytes.len() as u32;
                break;
            }
            i += 1;
        }
        total
    };
    // Imported functions occupy wasm idxs 0..imports.len(); typeck
    // assigned non-generic entries idxs starting at imports.len(); so
    // mono picks up after both.
    let mono_start = wasm_mod.imports.len() as u32 + funcs.entries.len() as u32;
    let mut mono = MonoState::new(mono_start, str_pool_base_offset);
    emit_module(wasm_mod, root, &mut module_path, structs, enums, traits, funcs, &mut mono)?;
    while !mono.queue.is_empty() {
        let work = mono.queue.remove(0);
        emit_monomorphic(wasm_mod, work, structs, enums, traits, funcs, &mut mono)?;
    }
    // Flush this crate's pool contribution into the single segment at
    // STR_POOL_BASE — appending if a prior crate already created it,
    // creating fresh if not — and bump `__heap_top`'s init past the
    // new total.
    if !mono.str_pool_bytes.is_empty() {
        let mut found: Option<usize> = None;
        let mut i = 0;
        while i < wasm_mod.datas.len() {
            if wasm_mod.datas[i].offset == STR_POOL_BASE {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let new_total = match found {
            Some(idx) => {
                let mut k = 0;
                while k < mono.str_pool_bytes.len() {
                    wasm_mod.datas[idx].bytes.push(mono.str_pool_bytes[k]);
                    k += 1;
                }
                wasm_mod.datas[idx].bytes.len() as u32
            }
            None => {
                let segment_bytes = mono.str_pool_bytes.clone();
                let total = segment_bytes.len() as u32;
                wasm_mod.datas.push(wasm::Data {
                    offset: STR_POOL_BASE,
                    bytes: segment_bytes,
                });
                total
            }
        };
        wasm_mod.globals[1].init =
            wasm::Instruction::I32Const((STR_POOL_BASE as i32) + (new_total as i32));
    }
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

// Look up an inherent impl block's resolved target by reading the
// first method's stored `impl_target`. Used for non-Path inherent
// impls (`impl<T> *const T { … }`), where typeck already resolved the
// AST target at registration time. If the impl has no methods (rare
// but legal), returns a placeholder — there's nothing for codegen to
// emit anyway, so the value is unused.
fn recover_impl_target_from_methods(
    funcs: &FuncTable,
    method_prefix: &Vec<String>,
    methods: &Vec<crate::ast::Function>,
) -> RType {
    let mut k = 0;
    while k < methods.len() {
        let mut full = method_prefix.clone();
        full.push(methods[k].name.clone());
        // Look up in concrete entries first, then templates.
        let mut i = 0;
        while i < funcs.entries.len() {
            if funcs.entries[i].path == full {
                if let Some(t) = &funcs.entries[i].impl_target {
                    return t.clone();
                }
            }
            i += 1;
        }
        let mut i = 0;
        while i < funcs.templates.len() {
            if funcs.templates[i].path == full {
                if let Some(t) = &funcs.templates[i].impl_target {
                    return t.clone();
                }
            }
            i += 1;
        }
        k += 1;
    }
    // No methods (or none found) — the value is unused. Return unit
    // tuple as a safe placeholder.
    RType::Tuple(Vec::new())
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
    // Value's bytes live at `[addr_local + 0]` — used for pattern
    // bindings that escape analysis flagged as addressed. The slot is
    // dynamically allocated at bind time (`__sp -= byte_size`); the
    // function epilogue's saved-SP restore reclaims it. Reads/writes
    // / address-takes go through `addr_local`. Mirrors `Memory`'s
    // role for let-bindings but with an i32-local-held base address
    // instead of a fixed `__sp + frame_offset`.
    MemoryAt { addr_local: u32 },
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
    enums: &'a crate::typeck::EnumTable,
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
    // Per-pattern-binding (Binding/At Pattern.id) flag — `true` means the
    // binding is borrowed somewhere in the arm, so `bind_pattern_value`
    // allocates a shadow-stack slot at bind time (Storage::MemoryAt).
    // Indexed by Pattern.id. Sized to `func.node_count`.
    pattern_addressed: Vec<bool>,
    method_resolutions: Vec<Option<MethodResolution>>,
    call_resolutions: Vec<Option<CallResolution>>,
    // Per-NodeId resolved type-args for builtins that need them at codegen
    // (currently only `¤size_of::<T>()`). Substituted through the mono env
    // by the caller; codegen reads with `expr.id` and uses
    // `byte_size_of(T, structs, enums)`.
    builtin_type_targets: Vec<Option<Vec<RType>>>,
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
    // Stack of enclosing loops (innermost-last). Each frame records the
    // wasm structured-control-flow depth at the loop's entry, used to
    // compute the right `Br` index for break/continue. `loop_depth` is
    // the depth of the wasm `Loop` instruction (= continue target);
    // `break_depth` is the depth of the wrapping `Block` (= break
    // target).
    loops: Vec<LoopCgFrame>,
    // Current wasm structured-control-flow depth (number of open
    // structured constructs: Block, Loop, If). `Br(N)` jumps to the
    // construct at depth `N` from the innermost. break/continue
    // compute their `Br` index as `depth_at_emit - frame.depth - 1`.
    cf_depth: u32,
    // Wasm local holding `__sp` immediately after the prologue's
    // `__sp -= frame_size` subtraction. Spilled-binding accesses
    // (BaseAddr::StackPointer) emit `LocalGet(frame_base_local)` so
    // they're immune to subsequent `__sp` drift from `&literal` borrow
    // temps, enum construction, sret allocations, etc.
    frame_base_local: u32,
    // Wasm local holding the saved `__sp` from function entry. Used by
    // `return` to restore SP before the wasm `Return` instruction;
    // also used at function-end as the natural epilogue's SP source.
    fn_entry_sp_local: u32,
    // For sret-returning functions: the wasm local idx of the
    // caller-supplied destination address (always wasm local 0).
    // `None` for non-sret functions.
    sret_ptr_local: Option<u32>,
    // The function's flat return type (in wasm scalar order). Empty
    // for unit-returning fns; `[I32]` for simple scalar returns;
    // `[I32, I32]` for fat-ref returns; etc. For sret-returning
    // functions this is `[I32]` (the sret address). Used by `return`
    // to know the shape it must produce.
    return_flat: Vec<wasm::ValType>,
    // The function's full return RType (post-mono substitution). Used
    // by `return` to decide the sret memcpy shape and by `?` to find
    // the function's Result-error type.
    return_rt: Option<RType>,
}

struct LoopCgFrame {
    label: Option<String>,
    // wasm structured-cf depth of the wrapping Block (= break target).
    break_depth: u32,
    // wasm structured-cf depth of the Loop instruction (= continue target).
    continue_depth: u32,
    // Number of FnCtx.locals at loop entry. break/continue emit drops
    // for any in-loop bindings (locals at indices ≥ this) before jumping.
    locals_len_at_entry: usize,
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
    enums: &crate::typeck::EnumTable,
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
        RType::Ref { inner, .. } => match inner.as_ref() {
            // Fat ref to a DST slice or str: two i32 leaves (data ptr, length).
            RType::Slice(_) | RType::Str => {
                out.push(MemLeaf {
                    byte_offset: base_offset,
                    byte_size: 4,
                    signed: false,
                    valtype: wasm::ValType::I32,
                });
                out.push(MemLeaf {
                    byte_offset: base_offset + 4,
                    byte_size: 4,
                    signed: false,
                    valtype: wasm::ValType::I32,
                });
            }
            _ => out.push(MemLeaf {
                byte_offset: base_offset,
                byte_size: 4,
                signed: false,
                valtype: wasm::ValType::I32,
            }),
        },
        RType::RawPtr { .. } => out.push(MemLeaf {
            byte_offset: base_offset,
            byte_size: 4,
            signed: false,
            valtype: wasm::ValType::I32,
        }),
        RType::Slice(_) | RType::Str => unreachable!("`[T]` / `str` is unsized — only valid behind a reference"),
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = make_struct_env(&entry.type_params, type_args);
            let mut off = base_offset;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                collect_leaves(&fty, structs, enums, off, out);
                off += byte_size_of(&fty, structs, enums);
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
        RType::Tuple(elems) => {
            // Tuple layout matches struct layout: tightly packed in
            // declaration order, no alignment padding. The unit type
            // `()` has no leaves at all.
            let mut off = base_offset;
            let mut i = 0;
            while i < elems.len() {
                collect_leaves(&elems[i], structs, enums, off, out);
                off += byte_size_of(&elems[i], structs, enums);
                i += 1;
            }
        }
        // Enums lay out as i32 disc + max-payload bytes. From the
        // outside the value is referenced by an i32 address; we don't
        // expose the inner leaves to the load/store-to-flat machinery
        // (which is for register-passable types). Codegen of enum
        // construction/match handles disc/payload bytes directly.
        RType::Enum { .. } => out.push(MemLeaf {
            byte_offset: base_offset,
            byte_size: 4,
            signed: false,
            valtype: wasm::ValType::I32,
        }),
        RType::AssocProj { .. } => unreachable!(
            "collect_leaves on unresolved associated-type projection — typeck should have concretized"
        ),
        // `!` has zero leaves — a value of type `!` never exists, so
        // there's nothing to lay out in memory.
        RType::Never => {}
        RType::Char => out.push(MemLeaf {
            byte_offset: base_offset,
            byte_size: 4,
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
    // Indexed by Pattern.id of the binding pattern node (a `Binding` or
    // `At` pattern). Same semantics as `let_addressed` but for match-arm
    // and (later) if-let pattern bindings: when a binding is borrowed
    // (`&n` of the binding, autoref of a method receiver, `&n.field`,
    // etc.), `bind_pattern_value` allocates a shadow-stack slot for it
    // so the address is stable; otherwise the binding sits in wasm
    // locals.
    pattern_addressed: Vec<bool>,
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
    // Carries the let's value expr NodeId (used to key let_addressed /
    // let_offsets).
    Let(u32, String),
    // Match / if-let pattern binding. NodeId is the Binding/At
    // Pattern's id (used to key `pattern_addressed`). Mirrors `Let`
    // for the address-analysis pass — we want `&binding.field…`
    // chains rooted at a pattern binding to mark the pattern slot
    // addressed so codegen spills it to the shadow stack from the
    // start.
    Pattern(u32, String),
}

fn binding_ref_name<'a>(b: &'a BindingRef) -> &'a str {
    match b {
        BindingRef::Param(_, n)
        | BindingRef::Let(_, n)
        | BindingRef::Pattern(_, n) => n,
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
                let root = &chain[0];
                let mut i = stack.len();
                while i > 0 {
                    i -= 1;
                    if binding_ref_name(&stack[i]) == root {
                        match &stack[i] {
                            BindingRef::Param(idx, _) => info.param_addressed[*idx] = true,
                            BindingRef::Let(id, _) => info.let_addressed[*id as usize] = true,
                            BindingRef::Pattern(id, _) => {
                                info.pattern_addressed[*id as usize] = true
                            }
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
                            BindingRef::Pattern(id, _) => {
                                info.pattern_addressed[*id as usize] = true
                            }
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
            // the Index/IndexMut method call. Mark the base's root
            // binding as addressed so escape analysis spills it.
            if let Some(chain) = extract_place(base) {
                let root = &chain[0];
                let mut i = stack.len();
                while i > 0 {
                    i -= 1;
                    if binding_ref_name(&stack[i]) == root {
                        match &stack[i] {
                            BindingRef::Param(idx, _) => info.param_addressed[*idx] = true,
                            BindingRef::Let(id, _) => info.let_addressed[*id as usize] = true,
                            BindingRef::Pattern(id, _) => {
                                info.pattern_addressed[*id as usize] = true
                            }
                        }
                        break;
                    }
                }
            }
            walk_expr_addr(base, stack, info);
            walk_expr_addr(index, stack, info);
        }
    }
}

// Push BindingRef::Pattern entries for every binding that `pattern`
// introduces, so address analysis on the arm body can resolve `&name`
// chains rooted at a pattern binding.
fn push_pattern_bindings_addr(pattern: &Pattern, stack: &mut Vec<BindingRef>) {
    use crate::ast::PatternKind;
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

// ============================================================================
// Module / function emission
// ============================================================================

fn emit_module(
    wasm_mod: &mut wasm::Module,
    module: &Module,
    path: &mut Vec<String>,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
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
                    emit_function(wasm_mod, f, path, path, None, structs, enums, traits, funcs, mono)?;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                emit_module(wasm_mod, m, path, structs, enums, traits, funcs, mono)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Impl(ib) => {
                // Determine the method-path prefix that mirrors what typeck
                // stored. For Path-targeted impls (`impl Foo`, `impl Trait
                // for Foo`), the prefix is the path's first segment. For
                // non-Path trait impls (`impl Trait for (u32, u32)`,
                // `impl<T> Trait for &T`, …) typeck synthesizes
                // `__trait_impl_<idx>` where `idx` is the impl's row in
                // the TraitTable; we recover that idx via the impl's
                // (file, span) identity. The `target_rt` for codegen's
                // `Self` resolution comes straight from the registered
                // `TraitImplEntry.target` — no need to re-resolve here.
                let target_name = match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
                        Some(p.segments[0].name.clone())
                    }
                    _ => None,
                };
                let trait_impl_idx = if ib.trait_path.is_some() {
                    crate::typeck::find_trait_impl_idx_by_span(
                        traits,
                        &module.source_file,
                        &ib.span,
                    )
                } else {
                    None
                };
                let mut method_prefix = path.clone();
                let mut target_path = path.clone();
                // Generic-trait impls on Path targets get an extra
                // `__trait_impl_<idx>` segment so multiple
                // `impl Trait<X> for Foo` rows have distinct paths.
                let trait_is_generic = trait_impl_idx.map_or(false, |idx| {
                    !traits.impls[idx].trait_args.is_empty()
                });
                let target_rt: RType = match &target_name {
                    Some(name) => {
                        method_prefix.push(name.clone());
                        if trait_is_generic {
                            if let Some(idx) = trait_impl_idx {
                                method_prefix.push(format!("__trait_impl_{}", idx));
                            }
                        }
                        target_path.push(name.clone());
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
                        RType::Struct {
                            path: target_path,
                            type_args: impl_param_args,
                            lifetime_args: impl_lifetime_args,
                        }
                    }
                    None => {
                        // Non-Path target: trait impl row idx, or a
                        // synth idx for inherent impls (recovered from
                        // setup's `(file, span)`-keyed table). For
                        // inherent impls, target_rt comes from the
                        // first method's stored `impl_target` — typeck
                        // already resolved the AST type and stored it.
                        if let Some(idx) = trait_impl_idx {
                            method_prefix.push(format!("__trait_impl_{}", idx));
                            traits.impls[idx].target.clone()
                        } else {
                            let synth_idx = crate::typeck::find_inherent_synth_idx(
                                funcs,
                                &module.source_file,
                                &ib.span,
                            )
                            .expect("setup recorded an inherent-synth idx for this impl");
                            method_prefix.push(format!("__inherent_synth_{}", synth_idx));
                            recover_impl_target_from_methods(funcs, &method_prefix, &ib.methods)
                        }
                    }
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
                            enums,
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
    enums: &crate::typeck::EnumTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let tmpl = &funcs.templates[work.template_idx];
    let env = build_env(&tmpl.type_params, &work.type_args);
    let param_types = subst_vec(&tmpl.param_types, &env);
    let return_type = tmpl.return_type.as_ref().map(|t| substitute_rtype(t, &env));
    let expr_types = subst_opt_vec(&tmpl.expr_types, &env);
    let method_resolutions = subst_opt_method_resolutions(&tmpl.method_resolutions, &env);
    let call_resolutions = subst_opt_call_resolutions(&tmpl.call_resolutions, &env);
    let builtin_type_targets = subst_opt_vec_vec(&tmpl.builtin_type_targets, &env);
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
        enums,
        traits,
        funcs,
        mono,
        param_types,
        return_type,
        expr_types,
        method_resolutions,
        call_resolutions,
        builtin_type_targets,
        moved_places,
        move_sites,
        env,
        tmpl.type_params.clone(),
        work.wasm_idx,
        false, // monomorphic instances are never exported
    )
}

fn subst_opt_vec_vec(
    v: &Vec<Option<Vec<RType>>>,
    env: &Vec<(String, RType)>,
) -> Vec<Option<Vec<RType>>> {
    let mut out: Vec<Option<Vec<RType>>> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        match &v[i] {
            Some(ts) => out.push(Some(subst_vec(ts, env))),
            None => out.push(None),
        }
        i += 1;
    }
    out
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

fn subst_opt_method_resolutions(
    v: &Vec<Option<MethodResolution>>,
    env: &Vec<(String, RType)>,
) -> Vec<Option<MethodResolution>> {
    let mut out: Vec<Option<MethodResolution>> = Vec::new();
    for entry in v {
        out.push(entry.as_ref().map(|m| {
            let mut cloned = m.clone();
            cloned.type_args = subst_vec(&m.type_args, env);
            if let Some(td) = &m.trait_dispatch {
                cloned.trait_dispatch = Some(crate::typeck::TraitDispatch {
                    trait_path: td.trait_path.clone(),
                    trait_args: subst_vec(&td.trait_args, env),
                    method_name: td.method_name.clone(),
                    recv_type: substitute_rtype(&td.recv_type, env),
                });
            }
            cloned
        }));
    }
    out
}

fn subst_opt_call_resolutions(
    v: &Vec<Option<CallResolution>>,
    env: &Vec<(String, RType)>,
) -> Vec<Option<CallResolution>> {
    let mut out: Vec<Option<CallResolution>> = Vec::new();
    for entry in v {
        out.push(entry.as_ref().map(|cr| match cr {
            CallResolution::Direct(idx) => CallResolution::Direct(*idx),
            CallResolution::Generic { template_idx, type_args } => CallResolution::Generic {
                template_idx: *template_idx,
                type_args: subst_vec(type_args, env),
            },
            CallResolution::Variant { enum_path, disc, type_args } => CallResolution::Variant {
                enum_path: enum_path.clone(),
                disc: *disc,
                type_args: subst_vec(type_args, env),
            },
        }));
    }
    out
}

fn build_env(type_params: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < type_params.len() {
        env.push((type_params[i].clone(), type_args[i].clone()));
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

fn emit_function(
    wasm_mod: &mut wasm::Module,
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let mut full = path_prefix.clone();
    full.push(func.name.clone());
    let entry = func_lookup(funcs, &full).expect("typeck registered this function");
    // Snapshot all artifacts before entering the concrete emitter (which takes
    // them by-value). For non-generic fns these are the entry's data; the env
    // is empty (no Param substitution to do).
    let param_types = entry.param_types.clone();
    let return_type = entry.return_type.clone();
    let expr_types = entry.expr_types.clone();
    let method_resolutions = entry.method_resolutions.clone();
    let call_resolutions = entry.call_resolutions.clone();
    let builtin_type_targets = clone_btt(&entry.builtin_type_targets);
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
        enums,
        traits,
        funcs,
        mono,
        param_types,
        return_type,
        expr_types,
        method_resolutions,
        call_resolutions,
        builtin_type_targets,
        moved_places,
        move_sites,
        Vec::new(),
        Vec::new(),
        wasm_idx,
        is_export,
    )
}

fn clone_btt(v: &Vec<Option<Vec<RType>>>) -> Vec<Option<Vec<RType>>> {
    let mut out: Vec<Option<Vec<RType>>> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        match &v[i] {
            Some(ts) => out.push(Some(ts.clone())),
            None => out.push(None),
        }
        i += 1;
    }
    out
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
    enums: &crate::typeck::EnumTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
    param_types: Vec<RType>,
    return_type: Option<RType>,
    expr_types: Vec<Option<RType>>,
    method_resolutions: Vec<Option<MethodResolution>>,
    call_resolutions: Vec<Option<CallResolution>>,
    builtin_type_targets: Vec<Option<Vec<RType>>>,
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
            frame_size += byte_size_of(&param_types[k], structs, enums);
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
                frame_size += byte_size_of(ty, structs, enums);
            }
            k += 1;
        }
    }

    // Build the WASM signature: refs collapse to a single i32; everything else
    // flattens to flat scalars as before. Functions returning enums use sret:
    // a leading i32 param is prepended (the destination address into the
    // caller's frame), and at function-body end we memcpy the constructed
    // enum's bytes to that address before returning.
    let returns_enum = matches!(&return_type, Some(RType::Enum { .. }));
    let mut wasm_params: Vec<wasm::ValType> = Vec::new();
    let mut next_wasm_local: u32 = 0;
    let mut sret_ptr_local: Option<u32> = None;
    if returns_enum {
        wasm_params.push(wasm::ValType::I32);
        sret_ptr_local = Some(next_wasm_local);
        next_wasm_local += 1;
    }
    let mut locals: Vec<LocalBinding> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let pty = param_types[k].clone();
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

    let return_flat_for_ctx: Vec<wasm::ValType> = wasm_results
        .iter()
        .map(|v| v.copy())
        .collect();

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
        enums,
        traits,
        funcs,
        current_module: current_module.clone(),
        expr_types,
        let_offsets,
        pattern_addressed: address_info.pattern_addressed.clone(),
        method_resolutions,
        call_resolutions,
        builtin_type_targets,
        self_target: self_target.cloned(),
        moved_places,
        move_sites,
        drop_flags: Vec::new(),
        pending_types: Vec::new(),
        pending_types_base: wasm_mod.types.len() as u32,
        env,
        type_params,
        mono,
        loops: Vec::new(),
        cf_depth: 0,
        frame_base_local: 0,
        fn_entry_sp_local: 0,
        sret_ptr_local,
        return_flat: return_flat_for_ctx,
        return_rt: return_type.clone(),
    };

    // Allocate a wasm local to remember __sp on function entry. Variant
    // construction sites allocate fresh slots dynamically via `__sp -=
    // size`; the epilogue restores from this saved value rather than
    // doing a fixed `__sp += frame_size`. This way the function ends
    // with __sp restored regardless of how many enum temporaries got
    // allocated during the body.
    let fn_entry_sp_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.fn_entry_sp_local = fn_entry_sp_local;
    ctx.instructions
        .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions
        .push(wasm::Instruction::LocalSet(fn_entry_sp_local));

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
    }

    // Save the post-prologue __sp into a wasm local. All spilled-binding
    // reads/writes go through this stable base, so subsequent __sp drift
    // (from `&literal` temps, enum construction, etc.) doesn't shift
    // where the bindings appear. Allocated regardless of frame_size so
    // BaseAddr::StackPointer always has a valid base to load.
    {
        let fb_local = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.frame_base_local = fb_local;
        ctx.instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::LocalSet(fb_local));
    }

    if frame_size > 0 {

        // Copy each spilled param from its incoming WASM-local slot into memory.
        // Scan locals[] in declaration order — params are first, in order.
        // For enum-typed params the incoming wasm value is the enum's
        // address (caller's frame); we memcpy the disc + payload bytes
        // into the slot so the local owns its own copy. This matches
        // `store_flat_to_memory`'s inline representation for spilled
        // let-bindings — `load_flat_from_memory` for an enum then
        // produces the slot's own address regardless of source.
        //
        // The cursor tracks the wasm local index of the next user-param
        // scalar. For sret-returning functions, wasm local 0 is the
        // caller-supplied sret_addr — user params start at local 1.
        // Without this offset, the spill prologue copies sret_addr into
        // the first spilled-param slot, corrupting `self`.
        let mut p = 0;
        let mut wasm_local_cursor: u32 = if returns_enum { 1 } else { 0 };
        while p < func.params.len() {
            let pty = ctx.locals[p].rtype.clone();
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&pty, structs, &mut vts);
            let flat_size = vts.len() as u32;
            match &param_offsets[p] {
                Some(off) => {
                    if matches!(&pty, RType::Enum { .. }) {
                        // memcpy from incoming-address (a single i32 in
                        // wasm_local_cursor) to frame[off]. flat_size
                        // is 1 for enums; the wasm local at cursor
                        // holds the source address.
                        let src_local = wasm_local_cursor;
                        let dst_local = ctx.next_wasm_local;
                        ctx.extra_locals.push(wasm::ValType::I32);
                        ctx.next_wasm_local += 1;
                        let fb = ctx.frame_base_local;
                        ctx.instructions.push(wasm::Instruction::LocalGet(fb));
                        if *off != 0 {
                            ctx.instructions
                                .push(wasm::Instruction::I32Const(*off as i32));
                            ctx.instructions.push(wasm::Instruction::I32Add);
                        }
                        ctx.instructions.push(wasm::Instruction::LocalSet(dst_local));
                        let bytes = byte_size_of(&pty, structs, enums);
                        emit_memcpy(&mut ctx, dst_local, src_local, bytes);
                    } else {
                        let mut leaves: Vec<MemLeaf> = Vec::new();
                        collect_leaves(&pty, structs, enums, 0, &mut leaves);
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
        let rt = ctx.locals[p].rtype.clone();
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
        if returns_enum {
            // sret: copy bytes from the constructed enum's address
            // (saved at save_start, an i32) into the caller-supplied
            // sret_ptr (wasm local 0). After SP restore, the temp
            // address is invalid; sret_ptr is in the caller's frame
            // and stays valid. We then push sret_ptr as the i32
            // return value so the caller keeps a stable address.
            let enum_ty = return_type.as_ref().unwrap();
            let bytes = byte_size_of(enum_ty, structs, enums);
            emit_memcpy(
                &mut ctx,
                sret_ptr_local.expect("sret_ptr allocated when returns_enum"),
                save_start,
                bytes,
            );
            ctx.instructions.push(wasm::Instruction::LocalGet(
                sret_ptr_local.expect("sret_ptr allocated when returns_enum"),
            ));
        } else {
            let mut k = 0;
            while k < return_flat.len() {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(save_start + k as u32));
                k += 1;
            }
        }
    } else {
        let n = ctx.locals.len();
        emit_drops_for_locals_range(&mut ctx, 0, n)?;
    }

    // Epilogue: restore __sp to its function-entry value. This covers
    // both the static `frame_size` allocation from the prologue and any
    // dynamic slots allocated during the body (e.g. enum-construction
    // temporaries). The return value (if any) is already on the WASM
    // stack from the body's tail expression; SP arithmetic doesn't
    // touch the operand stack.
    ctx.instructions
        .push(wasm::Instruction::LocalGet(fn_entry_sp_local));
    ctx.instructions
        .push(wasm::Instruction::GlobalSet(SP_GLOBAL));

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
    // Two flavours land here:
    //  - tail-less block-like exprs (`unsafe { … }`, `{ … }`) — they
    //    produce nothing on the WASM stack, just side effects.
    //  - any other expression followed by `;` — we evaluate and then
    //    drop the resulting flat scalars (one `drop` per scalar).
    match &expr.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) if b.tail.is_none() => {
            codegen_unit_block_stmt(ctx, b.as_ref())
        }
        _ => {
            let ty = codegen_expr(ctx, expr)?;
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&ty, ctx.structs, &mut vts);
            let mut k = 0;
            while k < vts.len() {
                ctx.instructions.push(wasm::Instruction::Drop);
                k += 1;
            }
            Ok(())
        }
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
// Count the number of currently-open wasm structured-control-flow
// constructs (Block, Loop, If) above the current emission point. Each
// `End` closes one. `Else` doesn't change depth (it's just a separator
// inside an If).
//
// Used by break/continue codegen to compute the correct `Br` index
// without requiring every other codegen helper that pushes a Block/If
// to track depth manually.
fn current_cf_depth(ctx: &FnCtx) -> u32 {
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < ctx.instructions.len() {
        match &ctx.instructions[i] {
            wasm::Instruction::Block(_)
            | wasm::Instruction::Loop(_)
            | wasm::Instruction::If(_) => depth += 1,
            wasm::Instruction::End => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth as u32
}

// `while cond { body }` lowering. Wasm structure:
//
//   Block (Empty)              ; break target
//     Loop (Empty)             ; continue target
//       <cond>                 ; produces i32 (bool)
//       i32.eqz                ; invert: true→0, false→1
//       BrIf 1                 ; if !cond, exit outer Block
//       <body>                 ; iteration code
//       Br 0                   ; back-edge to Loop start
//     End
//   End
//
// `break` inside body: drop in-loop Drop bindings, then `Br <break_depth>`.
// `continue` inside body: drop in-loop Drop bindings, then `Br <continue_depth>`.
fn codegen_while_expr(ctx: &mut FnCtx, w: &crate::ast::WhileExpr) -> Result<RType, Error> {
    let outer_depth = current_cf_depth(ctx);
    let locals_at_entry = ctx.locals.len();

    ctx.instructions
        .push(wasm::Instruction::Block(wasm::BlockType::Empty));
    ctx.instructions
        .push(wasm::Instruction::Loop(wasm::BlockType::Empty));

    // Cond on top of stack, eqz, br_if 1 (out of Block on false).
    let _ = codegen_expr(ctx, &w.cond)?;
    ctx.instructions.push(wasm::Instruction::I32Eqz);
    ctx.instructions.push(wasm::Instruction::BrIf(1));

    // Push loop frame so break/continue inside body can find this loop.
    ctx.loops.push(LoopCgFrame {
        label: w.label.clone(),
        break_depth: outer_depth,
        continue_depth: outer_depth + 1,
        locals_len_at_entry: locals_at_entry,
    });
    codegen_unit_block_stmt(ctx, w.body.as_ref())?;
    ctx.loops.pop();

    // Back-edge to Loop start.
    ctx.instructions.push(wasm::Instruction::Br(0));

    ctx.instructions.push(wasm::Instruction::End); // close Loop
    ctx.instructions.push(wasm::Instruction::End); // close Block

    Ok(RType::Tuple(Vec::new()))
}

fn codegen_break(ctx: &mut FnCtx, label: Option<&str>) -> Result<RType, Error> {
    // Find target loop frame.
    let (frame_idx, locals_at_entry) = find_loop_frame(ctx, label)
        .expect("typeck verified break has a target");
    let break_depth = ctx.loops[frame_idx].break_depth;
    // Emit drops for in-loop Drop bindings before the Br.
    emit_drops_for_locals_range(ctx, locals_at_entry, ctx.locals.len())?;
    let cur = current_cf_depth(ctx);
    // br_index = (cf_depth - outer_depth) - 1, where outer_depth =
    // break_depth (cf depth at loop entry, just before the Block push).
    let br_idx = cur.saturating_sub(break_depth + 1);
    ctx.instructions.push(wasm::Instruction::Br(br_idx));
    // `break` has type `!` — wasm validator treats post-Br code as
    // polymorphic, so a `Br` standing in for any expected result type
    // (the if/match's BlockType) is accepted.
    Ok(RType::Never)
}

fn codegen_continue(ctx: &mut FnCtx, label: Option<&str>) -> Result<RType, Error> {
    let (frame_idx, locals_at_entry) = find_loop_frame(ctx, label)
        .expect("typeck verified continue has a target");
    let continue_depth = ctx.loops[frame_idx].continue_depth;
    emit_drops_for_locals_range(ctx, locals_at_entry, ctx.locals.len())?;
    let cur = current_cf_depth(ctx);
    let br_idx = cur.saturating_sub(continue_depth + 1);
    ctx.instructions.push(wasm::Instruction::Br(br_idx));
    Ok(RType::Never)
}

// `return EXPR` / `return`. Mirrors the function-end epilogue:
// 1. Codegen the value (or unit). Stash to fresh wasm locals so we
//    can run drops without disturbing the value.
// 2. Drop every in-scope binding (whole `ctx.locals`).
// 3. For sret-returning functions: memcpy the value's bytes to the
//    caller-supplied sret slot, then push the sret slot's address.
// 4. For non-sret functions: push the stashed flat scalars back.
// 5. Restore SP from `fn_entry_sp_local`.
// 6. Emit wasm `Return`.
fn codegen_return(ctx: &mut FnCtx, value: Option<&Expr>) -> Result<RType, Error> {
    // Step 1: codegen the value (or push unit, which produces no
    // wasm scalars).
    let return_rt = match &ctx.return_rt {
        Some(rt) => rt.clone(),
        None => RType::Tuple(Vec::new()),
    };
    if let Some(e) = value {
        codegen_expr(ctx, e)?;
    }
    // Stash flat return scalars into fresh locals so drops don't
    // disturb them.
    let return_flat = ctx.return_flat.clone();
    let save_start = ctx.next_wasm_local;
    let mut i = 0;
    while i < return_flat.len() {
        ctx.extra_locals.push(return_flat[i].copy());
        ctx.next_wasm_local += 1;
        i += 1;
    }
    let mut k = return_flat.len();
    while k > 0 {
        k -= 1;
        ctx.instructions
            .push(wasm::Instruction::LocalSet(save_start + k as u32));
    }
    // Step 2: drop every in-scope binding.
    let n = ctx.locals.len();
    emit_drops_for_locals_range(ctx, 0, n)?;
    // Step 3/4: produce the wasm return value.
    let returns_enum = matches!(&return_rt, RType::Enum { .. });
    if returns_enum {
        let bytes = byte_size_of(&return_rt, ctx.structs, ctx.enums);
        let dst = ctx.sret_ptr_local.expect("sret_ptr present for enum returns");
        emit_memcpy(ctx, dst, save_start, bytes);
        ctx.instructions.push(wasm::Instruction::LocalGet(dst));
    } else {
        let mut k = 0;
        while k < return_flat.len() {
            ctx.instructions
                .push(wasm::Instruction::LocalGet(save_start + k as u32));
            k += 1;
        }
    }
    // Step 5: restore SP.
    ctx.instructions
        .push(wasm::Instruction::LocalGet(ctx.fn_entry_sp_local));
    ctx.instructions
        .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    // Step 6: wasm Return.
    ctx.instructions.push(wasm::Instruction::Return);
    Ok(RType::Never)
}

// `arr[idx]` in value position — synthesize the equivalent of
// `*<Index>::index(&arr, idx)`. Resolves the impl via `solve_impl`,
// emits the recv as a borrow of base (matching `&self`), the idx,
// and the call; the call returns `&Output` as one i32 (the
// pointer), and we then load the `Output`'s flat scalars from that
// address. Caller-context-aware variants for assign/borrow contexts
// live in `codegen_index_ref` and `codegen_index_assign`.
fn codegen_index_value(
    ctx: &mut FnCtx,
    base: &Expr,
    idx: &Expr,
    expr_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let (callee_idx, _ref_ret_rt) = resolve_index_callee(ctx, base, false);
    let result_ty = ctx.expr_types[expr_id as usize]
        .as_ref()
        .expect("typeck recorded the index expr's type")
        .clone();
    let result_ty = substitute_rtype(&result_ty, &ctx.env);
    // Receiver shape: if base is already a `&Self` (e.g. `s: &[T]`
    // calling `[T]::index(&self)`), pass it through; otherwise take
    // `&base`.
    emit_index_recv(ctx, base, false)?;
    codegen_expr(ctx, idx)?;
    ctx.instructions.push(wasm::Instruction::Call(callee_idx));
    // Stack now has an i32 (address of &Output). Load `Output` from
    // it.
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    load_flat_from_memory(ctx, &result_ty, BaseAddr::WasmLocal(addr_local), 0);
    Ok(result_ty)
}

// Push the right receiver value for an Index/IndexMut method call.
// If `base`'s type already matches the method's `&self` shape — i.e.
// `base` is `&T` (for Index) or `&mut T` (for IndexMut) — pass it
// through with `codegen_expr`. Otherwise (base is owned `T` or
// matches in some other ref-permutation) take `&base` / `&mut base`
// via `codegen_borrow`.
fn emit_index_recv(ctx: &mut FnCtx, base: &Expr, mutable: bool) -> Result<(), Error> {
    let base_rt = ctx.expr_types[base.id as usize]
        .as_ref()
        .expect("typeck recorded base type")
        .clone();
    let base_rt = substitute_rtype(&base_rt, &ctx.env);
    let already_right_ref = match (&base_rt, mutable) {
        (RType::Ref { mutable: false, .. }, false) => true,
        (RType::Ref { mutable: true, .. }, true) => true,
        // `&mut T` can downgrade to `&T` — pass through for Index.
        (RType::Ref { mutable: true, .. }, false) => true,
        _ => false,
    };
    if already_right_ref {
        codegen_expr(ctx, base)?;
    } else {
        codegen_borrow(ctx, base, mutable)?;
    }
    Ok(())
}

// Resolve the wasm idx + return type of the appropriate Index /
// IndexMut method for `base`'s type. `mutable=true` selects
// IndexMut::index_mut. Used by both value-position indexing and the
// (future) borrow / assign paths.
fn resolve_index_callee(
    ctx: &mut FnCtx,
    base: &Expr,
    mutable: bool,
) -> (u32, RType) {
    let base_rt = ctx.expr_types[base.id as usize]
        .as_ref()
        .expect("typeck recorded base type")
        .clone();
    let base_rt = substitute_rtype(&base_rt, &ctx.env);
    let lookup_rt = match &base_rt {
        RType::Ref { inner, .. } => (**inner).clone(),
        _ => base_rt.clone(),
    };
    let trait_path: Vec<String> = if mutable {
        vec!["std".to_string(), "ops".to_string(), "IndexMut".to_string()]
    } else {
        vec!["std".to_string(), "ops".to_string(), "Index".to_string()]
    };
    let method_name = if mutable { "index_mut" } else { "index" };
    let resolution = crate::typeck::solve_impl(&trait_path, &lookup_rt, ctx.traits, 0)
        .expect("typeck verified Index/IndexMut impl exists");
    let cand = crate::typeck::find_trait_impl_method(ctx.funcs, resolution.impl_idx, method_name)
        .expect("impl has the index/index_mut method");
    match cand {
        crate::typeck::MethodCandidate::Direct(i) => {
            let entry = &ctx.funcs.entries[i];
            let ret = entry.return_type.clone().expect("index returns a ref");
            (entry.idx, ret)
        }
        crate::typeck::MethodCandidate::Template(i) => {
            let tmpl = &ctx.funcs.templates[i];
            let impl_param_count = tmpl.impl_type_param_count;
            let mut concrete: Vec<RType> = Vec::new();
            let mut k = 0;
            while k < impl_param_count {
                let name = &tmpl.type_params[k];
                let bound = resolution
                    .subst
                    .iter()
                    .find(|s| s.0 == *name)
                    .map(|s| s.1.clone())
                    .expect("impl-param bound by solve_impl");
                concrete.push(bound);
                k += 1;
            }
            // Index / IndexMut have no method-level type-params.
            let tmpl_env = build_env(&tmpl.type_params, &concrete);
            let return_rt = substitute_rtype(
                tmpl.return_type.as_ref().expect("index returns"),
                &tmpl_env,
            );
            let idx = ctx.mono.intern(i, concrete);
            (idx, return_rt)
        }
    }
}

// `expr?` codegen. Inner is `Result<T, E>` (an enum); function's
// return is `Result<U, E>`. Lower as:
//
//   evaluate inner (pushes its address, since enums are address-passed)
//   stash to addr_local
//   load disc at [addr_local + 0]
//   if disc == OK_DISC:
//     read Ok payload at [addr_local + 4], push as result of expr?
//   else:
//     // Build Err(e) for the function return
//     allocate a fresh slot for the function's Result<U, E>
//     store disc=ERR_DISC at slot
//     memcpy inner Err payload to slot (offset 4)
//     codegen_return-style: memcpy slot to sret, restore SP, push, Return
//
// Lifted directly here (no early desugar) so the `?` token's span is
// the diagnostic source for any error.
fn codegen_try(
    ctx: &mut FnCtx,
    inner: &Expr,
    _id: crate::ast::NodeId,
) -> Result<RType, Error> {
    use crate::typeck::{IntKind, byte_size_of as bso};
    // Step 1: codegen the inner Result. It pushes an i32 (the address
    // of the enum's bytes).
    let inner_rt = codegen_expr(ctx, inner)?;
    // Resolve the Result<T, E> shape from the inner type.
    let (ok_ty, err_ty) = match &inner_rt {
        RType::Enum { type_args, .. } if type_args.len() == 2 => {
            (type_args[0].clone(), type_args[1].clone())
        }
        _ => unreachable!("typeck verified `?` operand is Result<T, E>"),
    };
    // Stash the address.
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    // Discriminants: Result is `Ok` = disc 0, `Err` = disc 1 (declaration
    // order in `lib/std/result.rs`).
    const OK_DISC: i32 = 0;
    // Read disc.
    ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
    ctx.instructions.push(wasm::Instruction::I32Load {
        align: 0,
        offset: 0,
    });
    ctx.instructions.push(wasm::Instruction::I32Const(OK_DISC));
    ctx.instructions.push(wasm::Instruction::I32Eq);
    // The Ok-path produces the Ok payload (flat scalars or address);
    // the Err-path diverges. The if's BlockType is the Ok-payload's
    // shape.
    let mut ok_flat: Vec<wasm::ValType> = Vec::new();
    crate::typeck::flatten_rtype(&ok_ty, ctx.structs, &mut ok_flat);
    let bt = match ok_flat.len() {
        0 => wasm::BlockType::Empty,
        1 => wasm::BlockType::Single(ok_flat[0].copy()),
        _ => {
            // Multi-value: register a FuncType.
            let ft = wasm::FuncType {
                params: Vec::new(),
                results: ok_flat.clone(),
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
    // ── Then branch (Ok) ──
    // Read the Ok payload from [addr_local + 4]. The payload is a
    // tuple-shaped variant `Ok(T)` — single field at offset 4.
    let ok_bytes = bso(&ok_ty, ctx.structs, ctx.enums);
    if ok_bytes > 0 {
        // Push the payload's flat scalars by reading from [addr+4].
        // Use the existing leaf-loading machinery: we have
        // `addr_local` and want to load `ok_ty` at offset 4.
        load_flat_from_memory(ctx, &ok_ty, BaseAddr::WasmLocal(addr_local), 4);
    }
    ctx.instructions.push(wasm::Instruction::Else);
    // ── Else branch (Err) — diverge with `return Err(e)` ──
    // The function returns Result<U, E>. We allocate a fresh slot,
    // write disc=Err and copy the Err payload, then memcpy to sret
    // and Return.
    let fn_ret_rt = match &ctx.return_rt {
        Some(rt) => rt.clone(),
        None => unreachable!("typeck verified function returns Result"),
    };
    let fn_ret_bytes = bso(&fn_ret_rt, ctx.structs, ctx.enums);
    // SP -= fn_ret_bytes
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::I32Const(fn_ret_bytes as i32));
    ctx.instructions.push(wasm::Instruction::I32Sub);
    ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    let new_addr = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::LocalSet(new_addr));
    // Store disc = Err at [new_addr + 0]. Err is variant index 1.
    ctx.instructions.push(wasm::Instruction::LocalGet(new_addr));
    ctx.instructions.push(wasm::Instruction::I32Const(1));
    ctx.instructions.push(wasm::Instruction::I32Store {
        align: 0,
        offset: 0,
    });
    // Copy err payload from [addr_local + 4] to [new_addr + 4].
    let err_bytes = bso(&err_ty, ctx.structs, ctx.enums);
    if err_bytes > 0 {
        // Compute src = addr_local + 4 → temp local; dst = new_addr +
        // 4 → temp local; emit_memcpy.
        let src_tmp = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        let dst_tmp = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
        ctx.instructions.push(wasm::Instruction::I32Const(4));
        ctx.instructions.push(wasm::Instruction::I32Add);
        ctx.instructions.push(wasm::Instruction::LocalSet(src_tmp));
        ctx.instructions.push(wasm::Instruction::LocalGet(new_addr));
        ctx.instructions.push(wasm::Instruction::I32Const(4));
        ctx.instructions.push(wasm::Instruction::I32Add);
        ctx.instructions.push(wasm::Instruction::LocalSet(dst_tmp));
        emit_memcpy(ctx, dst_tmp, src_tmp, err_bytes);
        // Suppress unused.
        let _ = (src_tmp, dst_tmp);
    }
    // Now mirror codegen_return: stash new_addr into a temp shaped
    // like the function's return (one i32, since the fn returns an
    // enum via sret), drop in-scope bindings, memcpy to sret, restore
    // SP, push sret addr, Return.
    let n = ctx.locals.len();
    emit_drops_for_locals_range(ctx, 0, n)?;
    let dst = ctx.sret_ptr_local.expect("sret_ptr present for enum returns");
    emit_memcpy(ctx, dst, new_addr, fn_ret_bytes);
    ctx.instructions.push(wasm::Instruction::LocalGet(dst));
    ctx.instructions
        .push(wasm::Instruction::LocalGet(ctx.fn_entry_sp_local));
    ctx.instructions
        .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::Return);
    // Close the if/else.
    ctx.instructions.push(wasm::Instruction::End);
    Ok(ok_ty)
}

fn find_loop_frame(ctx: &FnCtx, label: Option<&str>) -> Option<(usize, usize)> {
    match label {
        None => ctx
            .loops
            .last()
            .map(|f| (ctx.loops.len() - 1, f.locals_len_at_entry)),
        Some(name) => {
            let mut i = ctx.loops.len();
            while i > 0 {
                i -= 1;
                if ctx.loops[i].label.as_deref() == Some(name) {
                    return Some((i, ctx.loops[i].locals_len_at_entry));
                }
            }
            None
        }
    }
}

fn emit_drops_for_locals_range(ctx: &mut FnCtx, from: usize, to: usize) -> Result<(), Error> {
    let mut i = to;
    while i > from {
        i -= 1;
        let rt = ctx.locals[i].rtype.clone();
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
                        found = Some(resolution.subst[j].1.clone());
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
    // Use `frame_base_local`, not live `__sp`: by the time scope-end
    // drops fire, the body may have allocated literal-borrow temps,
    // sret slots, or enum constructions that drifted `__sp` below the
    // frame's true base. Spilled bindings sit at fixed offsets
    // relative to the post-prologue base, captured into
    // `frame_base_local`.
    let frame_offset = match &ctx.locals[idx].storage {
        Storage::Memory { frame_offset } => *frame_offset,
        _ => unreachable!("Drop binding must be address-marked"),
    };
    ctx.instructions
        .push(wasm::Instruction::LocalGet(ctx.frame_base_local));
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
    let value_ty = ctx.expr_types[value_id]
        .as_ref()
        .expect("typeck recorded the let's type")
        .clone();
    let frame_offset_opt = ctx.let_offsets[value_id];

    match frame_offset_opt {
        Some(frame_offset) => {
            // Spilled — store flat scalars into memory at SP+frame_offset.
            store_flat_to_memory(ctx, &value_ty, BaseAddr::StackPointer, frame_offset);
            ctx.locals.push(LocalBinding {
                name: let_stmt.name.clone(),
                rtype: value_ty.clone(),
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
                rtype: value_ty.clone(),
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
    // `arr[idx] = val` — synthesize the equivalent of
    // `*<IndexMut>::index_mut(&mut arr, idx) = val`. Codegen the
    // value, resolve the index_mut callee, push the address, and
    // store-through.
    if let ExprKind::Index { base, index, .. } = &assign.lhs.kind {
        let elem_ty = ctx.expr_types[assign.lhs.id as usize]
            .as_ref()
            .expect("typeck recorded LHS index type")
            .clone();
        let elem_ty = substitute_rtype(&elem_ty, &ctx.env);
        // Push rhs value.
        codegen_expr(ctx, &assign.rhs)?;
        // Stash flat scalars so we can compute the address afterward.
        let mut flat: Vec<wasm::ValType> = Vec::new();
        crate::typeck::flatten_rtype(&elem_ty, ctx.structs, &mut flat);
        let val_save_start = ctx.next_wasm_local;
        let mut k = 0;
        while k < flat.len() {
            ctx.extra_locals.push(flat[k].copy());
            ctx.next_wasm_local += 1;
            k += 1;
        }
        let mut k = flat.len();
        while k > 0 {
            k -= 1;
            ctx.instructions
                .push(wasm::Instruction::LocalSet(val_save_start + k as u32));
        }
        // Resolve index_mut and compute the destination address.
        let (callee_idx, _ret_rt) = resolve_index_callee(ctx, base, true);
        emit_index_recv(ctx, base, true)?;
        codegen_expr(ctx, index)?;
        ctx.instructions.push(wasm::Instruction::Call(callee_idx));
        let addr_local = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
        // Store the saved value to that address.
        let mut k = 0;
        while k < flat.len() {
            ctx.instructions
                .push(wasm::Instruction::LocalGet(val_save_start + k as u32));
            k += 1;
        }
        store_flat_to_memory(ctx, &elem_ty, BaseAddr::WasmLocal(addr_local), 0);
        return Ok(());
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

    let root_ty = ctx.locals[binding_idx].rtype.clone();
    let through_mut_ref = matches!(&root_ty, RType::Ref { mutable: true, .. });

    // Walk the chain to determine the byte offset and the target field's type.
    // For root types that are `&mut Struct`, peel off the ref; field offsets
    // are relative to the pointed-at value.
    let mut current_ty = if through_mut_ref {
        match &root_ty {
            RType::Ref { inner, .. } => (**inner).clone(),
            _ => unreachable!(),
        }
    } else {
        root_ty.clone()
    };
    let mut chain_offset: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        match &current_ty {
            RType::Struct { path, type_args, .. } => {
                let struct_path = path.clone();
                let struct_args = type_args.clone();
                let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
                let env = make_struct_env(&entry.type_params, &struct_args);
                let mut field_offset: u32 = 0;
                let mut found_field = false;
                let mut j = 0;
                while j < entry.fields.len() {
                    let fty = substitute_rtype(&entry.fields[j].ty, &env);
                    let s = byte_size_of(&fty, ctx.structs, ctx.enums);
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
            }
            RType::Tuple(elems) => {
                let elems = elems.clone();
                let idx: usize = chain[i]
                    .parse()
                    .expect("typeck verified tuple-index segment");
                let mut elem_offset: u32 = 0;
                let mut j = 0;
                while j < idx {
                    elem_offset += byte_size_of(&elems[j], ctx.structs, ctx.enums);
                    j += 1;
                }
                chain_offset += elem_offset;
                current_ty = elems[idx].clone();
            }
            _ => unreachable!("typeck verified chain navigates structs/tuples"),
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
            Storage::MemoryAt { addr_local } => {
                store_flat_to_memory(
                    ctx,
                    &current_ty,
                    BaseAddr::WasmLocal(*addr_local),
                    chain_offset,
                );
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
    let mut current_ty = ctx.locals[binding_idx].rtype.clone();
    let mut flat_off: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        match &current_ty {
            RType::Struct { path, type_args, .. } => {
                let struct_path = path.clone();
                let struct_args = type_args.clone();
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
            }
            RType::Tuple(elems) => {
                let elems = elems.clone();
                let idx: usize = chain[i]
                    .parse()
                    .expect("typeck verified tuple-index segment");
                let mut j = 0;
                while j < idx {
                    let mut vts: Vec<wasm::ValType> = Vec::new();
                    flatten_rtype(&elems[j], ctx.structs, &mut vts);
                    flat_off += vts.len() as u32;
                    j += 1;
                }
                current_ty = elems[idx].clone();
            }
            _ => unreachable!("typeck verified chain navigates structs/tuples"),
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
            ExprKind::TupleIndex { base, index, .. } => {
                fields.push(format!("{}", index));
                current = base;
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
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => (**inner).clone(),
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
            RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_off: u32 = 0;
        let mut found = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let s = byte_size_of(&fty, ctx.structs, ctx.enums);
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
            ExprKind::TupleIndex { base, index, .. } => {
                chain.push(format!("{}", index));
                current = base;
            }
            _ => return None,
        }
    }
}

fn codegen_expr(ctx: &mut FnCtx, expr: &Expr) -> Result<RType, Error> {
    match &expr.kind {
        ExprKind::IntLit(n) => {
            let ty = ctx.expr_types[expr.id as usize]
                .as_ref()
                .expect("typeck recorded this literal's type")
                .clone();
            emit_int_lit(ctx, &ty, *n, false);
            Ok(ty)
        }
        ExprKind::NegIntLit(n) => {
            let ty = ctx.expr_types[expr.id as usize]
                .as_ref()
                .expect("typeck recorded this literal's type")
                .clone();
            emit_int_lit(ctx, &ty, *n, true);
            Ok(ty)
        }
        ExprKind::CharLit(cp) => {
            // `char` flattens to one i32 — push the codepoint value.
            ctx.instructions.push(wasm::Instruction::I32Const(*cp as i32));
            Ok(RType::Char)
        }
        ExprKind::StrLit(s) => {
            // Intern into the module-wide pool; emit `i32.const ptr;
            // i32.const len` — the fat-ref representation of `&str`.
            let (addr, len) = ctx.mono.intern_str(s);
            ctx.instructions
                .push(wasm::Instruction::I32Const(addr as i32));
            ctx.instructions
                .push(wasm::Instruction::I32Const(len as i32));
            let ty = ctx.expr_types[expr.id as usize]
                .as_ref()
                .expect("typeck recorded this literal's type")
                .clone();
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
            let target = ctx.expr_types[expr.id as usize]
                .as_ref()
                .expect("typeck recorded the cast's target type")
                .clone();
            // Apply the monomorphization env in case the cast target
            // contains a `Param` (e.g. inside a generic body).
            let target = substitute_rtype(&target, &ctx.env);
            // T5: integer-to-integer casts may need wasm conversion ops.
            // i32-flatten ↔ i64 transitions emit wrap_i64 / extend_i32_*.
            // Same-flatten kinds (e.g. u8 ↔ i32) are no-ops since pocket-
            // rust stores all ≤32-bit integers in a wasm i32. Refs/raw
            // pointers are also i32 → no-op for those.
            //
            // `*T as <int>` — raw pointers flatten to i32 (wasm32
            // address). Treat the source as `usize` (an unsigned i32)
            // for sizing purposes; widen to i64 when the target is
            // 64-bit, drop the high half for 8/16-bit targets, etc.
            match (&src_ty, &target) {
                (RType::Int(src_k), RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, src_k, tgt_k);
                }
                (RType::RawPtr { .. }, RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, &IntKind::Usize, tgt_k);
                }
                // `char` and `u32` share the wasm-i32 representation
                // and our 4-byte storage. char→int and int→char are
                // bit-pattern reinterpretations — `as i64`-shaped
                // widening still needs `extend_i32_u` so we route
                // through `emit_int_to_int_cast` with the right kind.
                (RType::Char, RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, &IntKind::U32, tgt_k);
                }
                (RType::Int(src_k), RType::Char) => {
                    emit_int_to_int_cast(ctx, src_k, &IntKind::U32);
                }
                _ => {}
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
        ExprKind::Builtin { name, args, .. } => codegen_builtin(ctx, name, args, expr.id),
        ExprKind::Tuple(elems) => codegen_tuple_lit(ctx, elems),
        ExprKind::TupleIndex { base, index, .. } => codegen_tuple_index(ctx, base, *index),
        ExprKind::Match(m) => codegen_match_expr(ctx, m, expr.id),
        ExprKind::IfLet(il) => codegen_if_let_expr(ctx, il, expr.id),
        ExprKind::While(w) => codegen_while_expr(ctx, w),
        ExprKind::Break { label, .. } => codegen_break(ctx, label.as_deref()),
        ExprKind::Continue { label, .. } => codegen_continue(ctx, label.as_deref()),
        ExprKind::Return { value } => codegen_return(ctx, value.as_deref()),
        ExprKind::Try { inner, .. } => codegen_try(ctx, inner, expr.id),
        ExprKind::Index { base, index, .. } => codegen_index_value(ctx, base, index, expr.id),
        ExprKind::MacroCall { name, args, .. } => codegen_macro_call(ctx, name, args),
    }
}

// `panic!(msg)` — codegen the &str arg (pushes ptr, len), call the
// imported `env.panic` (wasm function index 0), then `unreachable`.
// The expression's "result" is `!` so wasm validator accepts the
// dead code that follows.
fn codegen_macro_call(
    ctx: &mut FnCtx,
    name: &str,
    args: &Vec<Expr>,
) -> Result<RType, Error> {
    if name != "panic" {
        unreachable!("typeck verified macro is `panic!`");
    }
    // The arg is `&str` — a 2-i32 fat ref. After codegen the wasm
    // stack has [ptr, len] which lines up with `env.panic(ptr, len)`.
    codegen_expr(ctx, &args[0])?;
    ctx.instructions.push(wasm::Instruction::Call(0));
    ctx.instructions.push(wasm::Instruction::Unreachable);
    Ok(RType::Never)
}

// `(a, b, c)` — codegen each elem in source order, leaving its
// flat scalars on the wasm stack. The tuple's flat representation
// is the concatenation; `()` produces no instructions at all.
fn codegen_tuple_lit(ctx: &mut FnCtx, elems: &Vec<Expr>) -> Result<RType, Error> {
    let mut elem_tys: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < elems.len() {
        let ty = codegen_expr(ctx, &elems[i])?;
        elem_tys.push(ty);
        i += 1;
    }
    Ok(RType::Tuple(elem_tys))
}

// `t.<index>` — analogous to `codegen_field_access`. Try the place-rooted
// path (chain bottoms at a Var or a chain through &/structs/tuples) for
// direct memory access; otherwise fall back to evaluating the whole base
// onto the stack and stash-extracting the element.
fn codegen_tuple_index(ctx: &mut FnCtx, base: &Expr, index: u32) -> Result<RType, Error> {
    let chain = {
        let mut tmp: Vec<String> = Vec::new();
        tmp.push(format!("{}", index));
        if collect_place_chain(base, &mut tmp) {
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
    let base_type = codegen_expr(ctx, base)?;
    extract_tuple_elem_from_stack(ctx, &base_type, index)
}

// Stack-position twin of `extract_field_from_stack`. The tuple's flat
// scalars are on the stack in declaration order; we need to keep only
// the slice belonging to element `index`.
fn extract_tuple_elem_from_stack(
    ctx: &mut FnCtx,
    base_type: &RType,
    index: u32,
) -> Result<RType, Error> {
    let elems: Vec<RType> = match base_type {
        RType::Tuple(elems) => elems.clone(),
        RType::Ref { inner, .. } => match inner.as_ref() {
            RType::Tuple(elems) => elems.clone(),
            _ => unreachable!("typeck rejects tuple-index on non-tuple"),
        },
        _ => unreachable!("typeck rejects tuple-index on non-tuple"),
    };
    let mut total_flat: u32 = 0;
    let mut field_flat_off: u32 = 0;
    let mut field_valtypes: Vec<wasm::ValType> = Vec::new();
    let mut field_ty: RType = RType::Int(IntKind::I32);
    let mut i = 0;
    while i < elems.len() {
        let mut vts: Vec<wasm::ValType> = Vec::new();
        flatten_rtype(&elems[i], ctx.structs, &mut vts);
        let s = vts.len() as u32;
        if i as u32 == index {
            field_flat_off = total_flat;
            field_valtypes = vts;
            field_ty = elems[i].clone();
        }
        total_flat += s;
        i += 1;
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

// Lower `¤name(args)` to its wasm op(s). Args are codegen'd in order
// (left-to-right); then the corresponding wasm instruction is
// emitted. The result type is the same as what typeck returned for
// this Builtin's NodeId — read it back from `ctx.expr_types` rather
// than re-deriving from the name (saves a parse).
fn codegen_builtin(
    ctx: &mut FnCtx,
    name: &str,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    // Typed intrinsics (alloc/free/cast) don't fit the `<int_kind>_<op>`
    // shape. Dispatch them here so split_builtin_name doesn't need to.
    match name {
        "alloc" => return codegen_builtin_alloc(ctx, args, node_id),
        "free" => return codegen_builtin_free(ctx, args, node_id),
        "cast" => return codegen_builtin_cast(ctx, args, node_id),
        "size_of" => return codegen_builtin_size_of(ctx, node_id),
        "make_slice" => return codegen_builtin_make_slice(ctx, args, node_id),
        "slice_len" => return codegen_builtin_slice_len(ctx, args, node_id),
        "slice_ptr" | "slice_mut_ptr" => {
            return codegen_builtin_slice_ptr(ctx, args, node_id);
        }
        // str_len reuses slice_len (same fat-ref shape, same drop-ptr-keep-len).
        "str_len" => return codegen_builtin_slice_len(ctx, args, node_id),
        // str_as_bytes is a 1-arg pure pass-through: `&str` and `&[u8]`
        // share the fat-ref representation, so codegenning the arg
        // already leaves (ptr, len) on the stack — the result.
        "str_as_bytes" => return codegen_builtin_passthrough_one(ctx, args, node_id),
        // make_str(ptr, len) builds a fat ref, exactly like make_slice.
        "make_str" => return codegen_builtin_make_slice(ctx, args, node_id),
        "make_mut_slice" => return codegen_builtin_make_slice(ctx, args, node_id),
        "ptr_usize_add" => return codegen_builtin_ptr_arith(ctx, args, node_id, PtrArith::Add),
        "ptr_usize_sub" => return codegen_builtin_ptr_arith(ctx, args, node_id, PtrArith::Sub),
        "ptr_isize_offset" => {
            return codegen_builtin_ptr_arith(ctx, args, node_id, PtrArith::Add);
        }
        _ => {}
    }
    // Codegen each arg in order. Each pushes its flattened scalar(s)
    // onto the stack. For 128-bit args, that's (low, high) — two i64s
    // per arg; codegen_builtin_128 takes care of the multi-scalar
    // unpack. For ≤64-bit, args produce a single scalar.
    let mut k = 0;
    while k < args.len() {
        codegen_expr(ctx, &args[k])?;
        k += 1;
    }
    let (ty_name, op) = split_builtin_name(name)
        .expect("typeck rejects unknown builtins before codegen");
    if matches!(ty_name, "u128" | "i128") {
        let signed = ty_name == "i128";
        codegen_builtin_128(ctx, op, signed);
        let ty = ctx.expr_types[node_id as usize]
            .as_ref()
            .expect("typeck recorded the builtin's result type")
            .clone();
        return Ok(ty);
    }
    // Pick the wasm class. bool and ≤32-bit ints use I32 ops; 64-bit
    // ints use I64 ops. Signed-vs-unsigned matters for div/rem and
    // for ordered comparisons (lt/le/gt/ge).
    let is_i64 = matches!(ty_name, "u64" | "i64");
    let is_signed = matches!(ty_name, "i8" | "i16" | "i32" | "i64" | "isize");
    let inst = match (is_i64, op) {
        (false, "add") => wasm::Instruction::I32Add,
        (false, "sub") => wasm::Instruction::I32Sub,
        (false, "mul") => wasm::Instruction::I32Mul,
        (false, "div") => {
            if is_signed { wasm::Instruction::I32DivS } else { wasm::Instruction::I32DivU }
        }
        (false, "rem") => {
            if is_signed { wasm::Instruction::I32RemS } else { wasm::Instruction::I32RemU }
        }
        (false, "and") => wasm::Instruction::I32And,
        (false, "or") => wasm::Instruction::I32Or,
        (false, "xor") => wasm::Instruction::I32Xor,
        (false, "eq") => wasm::Instruction::I32Eq,
        (false, "ne") => wasm::Instruction::I32Ne,
        (false, "lt") => {
            if is_signed { wasm::Instruction::I32LtS } else { wasm::Instruction::I32LtU }
        }
        (false, "le") => {
            if is_signed { wasm::Instruction::I32LeS } else { wasm::Instruction::I32LeU }
        }
        (false, "gt") => {
            if is_signed { wasm::Instruction::I32GtS } else { wasm::Instruction::I32GtU }
        }
        (false, "ge") => {
            if is_signed { wasm::Instruction::I32GeS } else { wasm::Instruction::I32GeU }
        }
        (false, "not") => {
            // Implemented as `i32.eqz` — turns 0 into 1 and any
            // nonzero (1) into 0.
            wasm::Instruction::I32Eqz
        }
        (true, "add") => wasm::Instruction::I64Add,
        (true, "sub") => wasm::Instruction::I64Sub,
        (true, "mul") => wasm::Instruction::I64Mul,
        (true, "div") => {
            if is_signed { wasm::Instruction::I64DivS } else { wasm::Instruction::I64DivU }
        }
        (true, "rem") => {
            if is_signed { wasm::Instruction::I64RemS } else { wasm::Instruction::I64RemU }
        }
        (true, "and") => wasm::Instruction::I64And,
        (true, "or") => wasm::Instruction::I64Or,
        (true, "xor") => wasm::Instruction::I64Xor,
        (true, "eq") => wasm::Instruction::I64Eq,
        (true, "ne") => wasm::Instruction::I64Ne,
        (true, "lt") => {
            if is_signed { wasm::Instruction::I64LtS } else { wasm::Instruction::I64LtU }
        }
        (true, "le") => {
            if is_signed { wasm::Instruction::I64LeS } else { wasm::Instruction::I64LeU }
        }
        (true, "gt") => {
            if is_signed { wasm::Instruction::I64GtS } else { wasm::Instruction::I64GtU }
        }
        (true, "ge") => {
            if is_signed { wasm::Instruction::I64GeS } else { wasm::Instruction::I64GeU }
        }
        _ => unreachable!("unknown builtin (typeck should have rejected): ¤{}", name),
    };
    ctx.instructions.push(inst);
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

// Lower a 128-bit builtin to a multi-instruction sequence. On entry,
// the wasm stack carries the two args' flattened forms in source
// order: bottom-to-top is `[low_a, high_a, low_b, high_b]`. Each arg
// is `(low: i64, high: i64)`, so 4 i64 slots in total. We pop them
// into fresh locals, compute, and push the result.
//
// add/sub: result is two i64s (low, high) on the stack — same flat
//   layout as the input. Signed and unsigned use the same bitwise
//   sequence (two's complement).
// eq/ne/lt/le/gt/ge: result is a single i32 (0 or 1).
//
// Not implemented: mul, div, rem (need wider runtime sequences;
// nothing in pocket-rust's bootstrap path needs them yet).
fn codegen_builtin_128(ctx: &mut FnCtx, op: &str, signed: bool) {
    // Allocate four i64 locals to hold the args.
    let low_a = alloc_i64_local(ctx);
    let high_a = alloc_i64_local(ctx);
    let low_b = alloc_i64_local(ctx);
    let high_b = alloc_i64_local(ctx);
    // Pop in reverse-push order: high_b is on top.
    ctx.instructions.push(wasm::Instruction::LocalSet(high_b));
    ctx.instructions.push(wasm::Instruction::LocalSet(low_b));
    ctx.instructions.push(wasm::Instruction::LocalSet(high_a));
    ctx.instructions.push(wasm::Instruction::LocalSet(low_a));
    match op {
        "add" => emit_128_add(ctx, low_a, high_a, low_b, high_b),
        "sub" => emit_128_sub(ctx, low_a, high_a, low_b, high_b),
        "eq" => emit_128_eq(ctx, low_a, high_a, low_b, high_b, /*invert=*/ false),
        "ne" => emit_128_eq(ctx, low_a, high_a, low_b, high_b, /*invert=*/ true),
        "lt" => emit_128_cmp(ctx, low_a, high_a, low_b, high_b, signed, Cmp128::Lt),
        "le" => emit_128_cmp(ctx, low_a, high_a, low_b, high_b, signed, Cmp128::Le),
        "gt" => emit_128_cmp(ctx, low_a, high_a, low_b, high_b, signed, Cmp128::Gt),
        "ge" => emit_128_cmp(ctx, low_a, high_a, low_b, high_b, signed, Cmp128::Ge),
        // mul/div/rem not yet implemented (need 64×64→128 widening
        // multiply or long-division). Emit wasm `unreachable` so the
        // function compiles cleanly; calling it traps at runtime.
        // Pocket-rust's bootstrap path doesn't multiply/divide u128.
        _ => {
            let _ = signed;
            ctx.instructions.push(wasm::Instruction::Unreachable);
        }
    }
}

// ¤alloc(n: usize) -> *mut u8. Bump-allocates `n` bytes from the heap.
// Currently a pure bump allocator — the heap grows up from offset 8;
// `¤free` doesn't reclaim. Out-of-memory traps as a normal wasm
// memory-access fault when the heap collides with the shadow stack
// (no explicit OOM check today).
//
// Wasm sequence:
//   <eval n>                ; stack: [n]
//   local.set n_temp        ; n_temp = n
//   global.get __heap_top   ; stack: [old]
//   local.set result_temp   ; result_temp = old
//   local.get result_temp   ; stack: [old]
//   local.get n_temp        ; stack: [old, n]
//   i32.add                 ; stack: [old + n]
//   global.set __heap_top   ; __heap_top = old + n
//   local.get result_temp   ; stack: [old]  -- return value
fn codegen_builtin_alloc(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    let n_temp = alloc_i32_local(ctx);
    let result_temp = alloc_i32_local(ctx);
    ctx.instructions.push(wasm::Instruction::LocalSet(n_temp));
    ctx.instructions.push(wasm::Instruction::GlobalGet(HEAP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::LocalSet(result_temp));
    ctx.instructions.push(wasm::Instruction::LocalGet(result_temp));
    ctx.instructions.push(wasm::Instruction::LocalGet(n_temp));
    ctx.instructions.push(wasm::Instruction::I32Add);
    ctx.instructions.push(wasm::Instruction::GlobalSet(HEAP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::LocalGet(result_temp));
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

// ¤free(p: *mut u8). No-op stub today — evaluates `p` for its side
// effects (move tracking, etc.) and discards the address. The heap is
// pure bump-allocation; freed memory is not reclaimed. Provided as the
// future hook point for a real allocator.
fn codegen_builtin_free(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    _node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    ctx.instructions.push(wasm::Instruction::Drop);
    Ok(RType::Tuple(Vec::new()))
}

// ¤cast::<A, B>(p: *X B) -> *X A (where X is const or mut, preserved).
// Pure no-op at runtime — raw pointers flatten to a single i32 address
// regardless of pointee type, so the wasm value passes through
// unchanged. Typeck has already validated the turbofish args.
fn codegen_builtin_cast(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

// `¤slice_ptr::<T>(s: &[T]) -> *const T` and the mut variant
// `¤slice_mut_ptr::<T>(s: &mut [T]) -> *mut T`. The arg pushes
// (data_ptr, len); we want `data_ptr` (below `len`) and discard
// `len` (top). One `drop` does it.
fn codegen_builtin_slice_ptr(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    ctx.instructions.push(wasm::Instruction::Drop);
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

// `¤slice_len::<T>(s: &[T]) -> usize`. The arg pushes (data_ptr, len)
// onto the stack; we want `len` (top) and discard `data_ptr` (below).
// Stash `len` to a temp local, drop the ptr, reload the temp.
fn codegen_builtin_slice_len(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    _node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    let len_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalSet(len_local));
    ctx.instructions.push(wasm::Instruction::Drop);
    ctx.instructions.push(wasm::Instruction::LocalGet(len_local));
    Ok(RType::Int(IntKind::Usize))
}

// 1-arg pass-through used by `¤str_as_bytes`: codegen the single arg
// (which already flattens to the desired result shape) and use the
// builtin's recorded result type.
fn codegen_builtin_passthrough_one(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

// `¤make_slice::<T>(ptr, len) -> &[T]`. Pure no-op at codegen: both
// args already flatten to one i32, leaving (ptr, len) on the wasm
// stack — exactly the fat-ref representation of `&[T]`.
fn codegen_builtin_make_slice(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    codegen_expr(ctx, &args[1])?;
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

// `¤size_of::<T>() -> usize`. Compile-time-constant: at this point T is
// concrete (after monomorphization), so we just emit `i32.const
// byte_size_of(T)`. The result type is `usize`, which flattens to an
// i32 on wasm32.
fn codegen_builtin_size_of(
    ctx: &mut FnCtx,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let ts = ctx.builtin_type_targets[node_id as usize]
        .as_ref()
        .expect("typeck recorded `¤size_of`'s T");
    let t = &ts[0];
    let size = byte_size_of(t, ctx.structs, ctx.enums);
    ctx.instructions
        .push(wasm::Instruction::I32Const(size as i32));
    Ok(RType::Int(IntKind::Usize))
}

#[derive(Copy, Clone)]
enum PtrArith {
    Add,
    Sub,
}

// `¤ptr_usize_add(p, n) -> *X T`, `¤ptr_usize_sub(p, n) -> *X T`,
// `¤ptr_isize_offset(p, n) -> *X T` — byte-wise pointer arithmetic.
// Raw pointers and usize/isize all flatten to wasm `i32` on wasm32, so
// each lowers to a single `i32.add` / `i32.sub`. Signed offsets use
// the same unsigned add (two's-complement: `p + (-1i32 as i32)` adds
// 0xFFFFFFFF, equivalent to `p - 1` in 32-bit arithmetic).
fn codegen_builtin_ptr_arith(
    ctx: &mut FnCtx,
    args: &Vec<Expr>,
    node_id: crate::ast::NodeId,
    op: PtrArith,
) -> Result<RType, Error> {
    codegen_expr(ctx, &args[0])?;
    codegen_expr(ctx, &args[1])?;
    let inst = match op {
        PtrArith::Add => wasm::Instruction::I32Add,
        PtrArith::Sub => wasm::Instruction::I32Sub,
    };
    ctx.instructions.push(inst);
    let ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded the builtin's result type")
        .clone();
    Ok(ty)
}

fn alloc_i32_local(ctx: &mut FnCtx) -> u32 {
    let idx = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    idx
}

fn alloc_i64_local(ctx: &mut FnCtx) -> u32 {
    let idx = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I64);
    ctx.next_wasm_local += 1;
    idx
}

// 128-bit add: low_r = low_a + low_b; carry = (low_r < low_a) ? 1 : 0;
// high_r = high_a + high_b + carry. Stack at end: [low_r, high_r].
fn emit_128_add(ctx: &mut FnCtx, low_a: u32, high_a: u32, low_b: u32, high_b: u32) {
    // Compute low_r and save it.
    ctx.instructions.push(wasm::Instruction::LocalGet(low_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_b));
    ctx.instructions.push(wasm::Instruction::I64Add);
    let low_r = alloc_i64_local(ctx);
    ctx.instructions.push(wasm::Instruction::LocalSet(low_r));
    // carry = (low_r < low_a) as u64 — overflow detection.
    ctx.instructions.push(wasm::Instruction::LocalGet(low_r));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_a));
    ctx.instructions.push(wasm::Instruction::I64LtU);
    ctx.instructions.push(wasm::Instruction::I64ExtendI32U);
    // high_r = high_a + high_b + carry.
    ctx.instructions.push(wasm::Instruction::LocalGet(high_a));
    ctx.instructions.push(wasm::Instruction::I64Add);
    ctx.instructions.push(wasm::Instruction::LocalGet(high_b));
    ctx.instructions.push(wasm::Instruction::I64Add);
    let high_r = alloc_i64_local(ctx);
    ctx.instructions.push(wasm::Instruction::LocalSet(high_r));
    // Push result in flat order: low first, then high.
    ctx.instructions.push(wasm::Instruction::LocalGet(low_r));
    ctx.instructions.push(wasm::Instruction::LocalGet(high_r));
}

// 128-bit sub: low_r = low_a - low_b; borrow = (low_a < low_b) ? 1 : 0;
// high_r = high_a - high_b - borrow. Sub is non-commutative, so we
// stash the borrow in a local and do `high_a - borrow - high_b` in
// stack order.
fn emit_128_sub(ctx: &mut FnCtx, low_a: u32, high_a: u32, low_b: u32, high_b: u32) {
    ctx.instructions.push(wasm::Instruction::LocalGet(low_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_b));
    ctx.instructions.push(wasm::Instruction::I64Sub);
    let low_r = alloc_i64_local(ctx);
    ctx.instructions.push(wasm::Instruction::LocalSet(low_r));
    // borrow = (low_a < low_b) → i32 → i64.
    ctx.instructions.push(wasm::Instruction::LocalGet(low_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_b));
    ctx.instructions.push(wasm::Instruction::I64LtU);
    ctx.instructions.push(wasm::Instruction::I64ExtendI32U);
    let borrow = alloc_i64_local(ctx);
    ctx.instructions.push(wasm::Instruction::LocalSet(borrow));
    // high_r = (high_a - borrow) - high_b.
    ctx.instructions.push(wasm::Instruction::LocalGet(high_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(borrow));
    ctx.instructions.push(wasm::Instruction::I64Sub);
    ctx.instructions.push(wasm::Instruction::LocalGet(high_b));
    ctx.instructions.push(wasm::Instruction::I64Sub);
    let high_r = alloc_i64_local(ctx);
    ctx.instructions.push(wasm::Instruction::LocalSet(high_r));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_r));
    ctx.instructions.push(wasm::Instruction::LocalGet(high_r));
}

// 128-bit eq: (low_a == low_b) AND (high_a == high_b). For ne, the
// caller passes invert=true and we flip the result with i32.eqz.
fn emit_128_eq(
    ctx: &mut FnCtx,
    low_a: u32,
    high_a: u32,
    low_b: u32,
    high_b: u32,
    invert: bool,
) {
    ctx.instructions.push(wasm::Instruction::LocalGet(low_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_b));
    ctx.instructions.push(wasm::Instruction::I64Eq);
    ctx.instructions.push(wasm::Instruction::LocalGet(high_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(high_b));
    ctx.instructions.push(wasm::Instruction::I64Eq);
    ctx.instructions.push(wasm::Instruction::I32And);
    if invert {
        ctx.instructions.push(wasm::Instruction::I32Eqz);
    }
}

#[derive(Clone, Copy)]
enum Cmp128 {
    Lt,
    Le,
    Gt,
    Ge,
}

// 128-bit ordered comparison. Decomposes into:
//   high comparison (signed for i128, unsigned for u128) OR
//   (high equal AND low comparison unsigned).
// For Le/Ge the low comparison uses ≤/≥; for Lt/Gt it uses </>.
fn emit_128_cmp(
    ctx: &mut FnCtx,
    low_a: u32,
    high_a: u32,
    low_b: u32,
    high_b: u32,
    signed: bool,
    cmp: Cmp128,
) {
    let (high_op, low_op) = match (signed, cmp) {
        (false, Cmp128::Lt) => (wasm::Instruction::I64LtU, wasm::Instruction::I64LtU),
        (false, Cmp128::Le) => (wasm::Instruction::I64LtU, wasm::Instruction::I64LeU),
        (false, Cmp128::Gt) => (wasm::Instruction::I64GtU, wasm::Instruction::I64GtU),
        (false, Cmp128::Ge) => (wasm::Instruction::I64GtU, wasm::Instruction::I64GeU),
        (true, Cmp128::Lt) => (wasm::Instruction::I64LtS, wasm::Instruction::I64LtU),
        (true, Cmp128::Le) => (wasm::Instruction::I64LtS, wasm::Instruction::I64LeU),
        (true, Cmp128::Gt) => (wasm::Instruction::I64GtS, wasm::Instruction::I64GtU),
        (true, Cmp128::Ge) => (wasm::Instruction::I64GtS, wasm::Instruction::I64GeU),
    };
    // (high_a OP high_b)
    ctx.instructions.push(wasm::Instruction::LocalGet(high_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(high_b));
    ctx.instructions.push(high_op);
    // OR
    // ((high_a == high_b) AND (low_a low_op low_b))
    ctx.instructions.push(wasm::Instruction::LocalGet(high_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(high_b));
    ctx.instructions.push(wasm::Instruction::I64Eq);
    ctx.instructions.push(wasm::Instruction::LocalGet(low_a));
    ctx.instructions.push(wasm::Instruction::LocalGet(low_b));
    ctx.instructions.push(low_op);
    ctx.instructions.push(wasm::Instruction::I32And);
    ctx.instructions.push(wasm::Instruction::I32Or);
}

fn split_builtin_name(name: &str) -> Option<(&str, &str)> {
    let ops = [
        "add", "sub", "mul", "div", "rem", "eq", "ne", "lt", "le", "gt", "ge",
        "and", "or", "not", "xor",
    ];
    let mut k = 0;
    while k < ops.len() {
        let op = ops[k];
        if name.len() > op.len() + 1 {
            let prefix_end = name.len() - op.len();
            if name.as_bytes()[prefix_end - 1] == b'_'
                && &name[prefix_end..] == op
            {
                return Some((&name[..prefix_end - 1], op));
            }
        }
        k += 1;
    }
    None
}

fn codegen_if_expr(
    ctx: &mut FnCtx,
    if_expr: &crate::ast::IfExpr,
    if_node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    // Evaluate the condition (an i32 0/1) onto the stack.
    let _ = codegen_expr(ctx, &if_expr.cond)?;
    let result_ty = ctx.expr_types[if_node_id as usize]
        .as_ref()
        .expect("typeck recorded the if's type")
        .clone();
    let mut flat: Vec<wasm::ValType> = Vec::new();
    crate::typeck::flatten_rtype(&result_ty, ctx.structs, &mut flat);
    let bt = match flat.len() {
        0 => wasm::BlockType::Empty,
        1 => wasm::BlockType::Single(flat[0]),
        _ => {
            // Multi-value `if` — register a FuncType (no params, these
            // results) and refer to it by index. We dedupe against
            // `ctx.pending_types`, then return base + idx so the typeidx
            // is correct after pending types are appended to
            // `wasm_mod.types` at function-emit-end.
            let ft = wasm::FuncType {
                params: Vec::new(),
                results: flat.clone(),
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

// `match scrut { pat => body, … }`. Lowering:
//
//   block (result T)              ; outer
//     block                       ; arm0_no_match
//       <pattern check 0>          ; if mismatch, br 0
//       <bind pattern names>
//       <body 0>                   ; pushes T
//       br 1                       ; deliver result to outer
//     end
//     block                       ; arm1_no_match
//       …
//     end
//     unreachable                 ; exhaustiveness ensures this is dead
//   end
//
// The scrutinee is evaluated once and stashed: a non-enum scrutinee
// goes into wasm locals (one per flat scalar); an enum scrutinee's
// wasm-stack value is an i32 address, which is saved to a single local
// and used as the base for `[addr + offset]` byte-addressed reads.
fn codegen_match_expr(
    ctx: &mut FnCtx,
    m: &crate::ast::MatchExpr,
    match_node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let result_ty = ctx.expr_types[match_node_id as usize]
        .as_ref()
        .expect("typeck recorded the match's type")
        .clone();
    let result_ty = substitute_rtype(&result_ty, &ctx.env);
    let scrut_ty = ctx.expr_types[m.scrutinee.id as usize]
        .as_ref()
        .expect("typeck recorded the scrutinee's type")
        .clone();
    let scrut_ty = substitute_rtype(&scrut_ty, &ctx.env);
    // Codegen the scrutinee (pushes its flat scalars or, for enums, its
    // address). Stash into wasm locals for stable read/re-read.
    let _ = codegen_expr(ctx, &m.scrutinee)?;
    // `ref` bindings need an addressable place. For enum scrutinees the
    // stash is already address-based (Memory). For non-enum scrutinees
    // we'd normally stash into wasm locals (Locals storage), but those
    // locals aren't addressable — so if any arm pattern uses `ref`, we
    // spill the scrutinee to a fresh shadow-stack slot first so every
    // sub-pattern's storage is Memory and `ref` bindings have a real
    // address. The function epilogue's saved-SP restore reclaims the
    // slot at function exit.
    let is_enum_scrut = matches!(&scrut_ty, RType::Enum { .. });
    let needs_ref_spill = !is_enum_scrut
        && m.arms.iter().any(|a| pattern_uses_ref_binding(&a.pattern));
    let storage = if needs_ref_spill {
        spill_match_scrutinee(ctx, &scrut_ty)
    } else {
        stash_match_scrutinee(ctx, &scrut_ty)
    };
    // Compute outer block-type for the unified arm result.
    let bt = block_type_for(ctx, &result_ty);
    ctx.instructions.push(wasm::Instruction::Block(bt));
    let mut i = 0;
    while i < m.arms.len() {
        let arm = &m.arms[i];
        ctx.instructions
            .push(wasm::Instruction::Block(wasm::BlockType::Empty));
        // Save locals/scope mark so pattern-introduced bindings clean
        // up after the arm body.
        let mark = ctx.locals.len();
        codegen_pattern(ctx, &arm.pattern, &scrut_ty, &storage, 0)?;
        // Guard: after pattern match succeeds, evaluate the bool
        // expression. If false, br to the arm's no-match label so
        // the next arm gets a chance.
        if let Some(g) = &arm.guard {
            let _ = codegen_expr(ctx, g)?;
            ctx.instructions.push(wasm::Instruction::I32Eqz);
            ctx.instructions.push(wasm::Instruction::BrIf(0));
        }
        // Body produces the result type's flat scalars on the stack.
        let _ = codegen_expr(ctx, &arm.body)?;
        ctx.instructions.push(wasm::Instruction::Br(1));
        ctx.instructions.push(wasm::Instruction::End);
        ctx.locals.truncate(mark);
        i += 1;
    }
    // After the last arm, exhaustiveness guarantees control would have
    // already exited — emit `unreachable` so the wasm validator sees
    // the outer block's expected result type without a fall-through.
    ctx.instructions.push(wasm::Instruction::Unreachable);
    ctx.instructions.push(wasm::Instruction::End);
    Ok(result_ty)
}

// Where the scrutinee value is parked between arms.
#[derive(Clone)]
enum PatScrut {
    /// Value's flat scalars live in consecutive wasm locals starting at
    /// `start`. The number of slots is `flatten_rtype(ty).len()` for the
    /// type at that storage. Sub-storage for tuple/struct fields is a
    /// sub-range: `Locals { start: start + flat_offset_to_child }`.
    Locals { start: u32 },
    /// Value's bytes live at `[addr_local + byte_offset]`. Used for
    /// enum-typed scrutinees: the base address is saved once on entry,
    /// and recursive descent into payload fields just bumps `byte_offset`.
    Memory { addr_local: u32, byte_offset: u32 },
}

// Walk a pattern recursively and return true if it (or any sub-pattern)
// is a `ref` binding. Used by codegen_match_expr to decide whether the
// non-enum scrutinee needs to be spilled to the shadow stack so that
// `ref` bindings have a real address.
// `if let Pat = scrut { then } else { else }`. Lowering mirrors a
// single-arm match plus the else fallback: outer wasm `block` carries
// the unified result type; an inner empty-result block holds the
// pattern check (br 0 on no-match → fall through to the else-block);
// on match the then-block runs and `br 1`s past the else with its
// result. The else-block runs only when the inner block fell through.
fn codegen_if_let_expr(
    ctx: &mut FnCtx,
    il: &crate::ast::IfLetExpr,
    if_let_node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    let result_ty = ctx.expr_types[if_let_node_id as usize]
        .as_ref()
        .expect("typeck recorded the if-let's type")
        .clone();
    let result_ty = substitute_rtype(&result_ty, &ctx.env);
    let scrut_ty = ctx.expr_types[il.scrutinee.id as usize]
        .as_ref()
        .expect("typeck recorded the scrutinee's type")
        .clone();
    let scrut_ty = substitute_rtype(&scrut_ty, &ctx.env);
    let _ = codegen_expr(ctx, &il.scrutinee)?;
    let is_enum_scrut = matches!(&scrut_ty, RType::Enum { .. });
    let needs_ref_spill = !is_enum_scrut && pattern_uses_ref_binding(&il.pattern);
    let storage = if needs_ref_spill {
        spill_match_scrutinee(ctx, &scrut_ty)
    } else {
        stash_match_scrutinee(ctx, &scrut_ty)
    };
    let outer_bt = block_type_for(ctx, &result_ty);
    ctx.instructions.push(wasm::Instruction::Block(outer_bt));
    ctx.instructions
        .push(wasm::Instruction::Block(wasm::BlockType::Empty));
    let mark = ctx.locals.len();
    codegen_pattern(ctx, &il.pattern, &scrut_ty, &storage, 0)?;
    let _ = codegen_block_expr(ctx, il.then_block.as_ref())?;
    ctx.instructions.push(wasm::Instruction::Br(1));
    ctx.locals.truncate(mark);
    ctx.instructions.push(wasm::Instruction::End);
    let _ = codegen_block_expr(ctx, il.else_block.as_ref())?;
    ctx.instructions.push(wasm::Instruction::End);
    Ok(result_ty)
}

fn pattern_uses_ref_binding(p: &Pattern) -> bool {
    use crate::ast::PatternKind;
    match &p.kind {
        PatternKind::Binding { by_ref, .. } => *by_ref,
        PatternKind::Wildcard
        | PatternKind::LitInt(_)
        | PatternKind::LitBool(_)
        | PatternKind::Range { .. } => false,
        PatternKind::At { inner, .. } => pattern_uses_ref_binding(inner),
        PatternKind::Or(alts) => alts.iter().any(|a| pattern_uses_ref_binding(a)),
        PatternKind::Ref { inner, .. } => pattern_uses_ref_binding(inner),
        PatternKind::Tuple(elems) => elems.iter().any(|e| pattern_uses_ref_binding(e)),
        PatternKind::VariantTuple { elems, .. } => {
            elems.iter().any(|e| pattern_uses_ref_binding(e))
        }
        PatternKind::VariantStruct { fields, .. } => {
            fields.iter().any(|f| pattern_uses_ref_binding(&f.pattern))
        }
    }
}

// Allocate a shadow-stack slot of `byte_size_of(ty)` bytes, copy the
// scrutinee's flat scalars from the wasm stack into the slot, and
// return Memory storage rooted at the slot. The slot is reclaimed by
// the function epilogue's saved-SP restore.
fn spill_match_scrutinee(ctx: &mut FnCtx, ty: &RType) -> PatScrut {
    let bytes = byte_size_of(ty, ctx.structs, ctx.enums);
    ctx.instructions
        .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions
        .push(wasm::Instruction::I32Const(bytes as i32));
    ctx.instructions.push(wasm::Instruction::I32Sub);
    ctx.instructions
        .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions
        .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    // Pop scrutinee's flat scalars and store them at addr_local. For
    // non-enum types this writes the flat representation. (Enum
    // scrutinees never reach here — they're already Memory-backed.)
    store_flat_to_memory(ctx, ty, BaseAddr::WasmLocal(addr_local), 0);
    PatScrut::Memory { addr_local, byte_offset: 0 }
}

fn stash_match_scrutinee(ctx: &mut FnCtx, ty: &RType) -> PatScrut {
    if matches!(ty, RType::Enum { .. }) {
        // Wasm-stack value is the i32 address. Stash into a local.
        let addr_local = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.instructions
            .push(wasm::Instruction::LocalSet(addr_local));
        PatScrut::Memory { addr_local, byte_offset: 0 }
    } else {
        let mut vts: Vec<wasm::ValType> = Vec::new();
        crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
        let start = ctx.next_wasm_local;
        let mut k = 0;
        while k < vts.len() {
            ctx.extra_locals.push(vts[k]);
            ctx.next_wasm_local += 1;
            k += 1;
        }
        // Pop in reverse order (top of stack is the LAST flat scalar).
        let mut k = vts.len();
        while k > 0 {
            k -= 1;
            ctx.instructions
                .push(wasm::Instruction::LocalSet(start + k as u32));
        }
        PatScrut::Locals { start }
    }
}

fn block_type_for(ctx: &mut FnCtx, ty: &RType) -> wasm::BlockType {
    let mut flat: Vec<wasm::ValType> = Vec::new();
    crate::typeck::flatten_rtype(ty, ctx.structs, &mut flat);
    match flat.len() {
        0 => wasm::BlockType::Empty,
        1 => wasm::BlockType::Single(flat[0]),
        _ => {
            let ft = wasm::FuncType {
                params: Vec::new(),
                results: flat.clone(),
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
    }
}

// Emit code that, given the scrutinee value at `storage`, either
// matches `pattern` (extracting any bindings into wasm locals registered
// in `ctx.locals`) or does `br no_match_target` to skip this arm.
//
// Bindings introduced by Ident/At/etc. become entries in `ctx.locals`
// so the body's codegen of `Var(name)` finds them. The caller's scope
// management (locals.truncate(mark)) drops them at arm end.
fn codegen_pattern(
    ctx: &mut FnCtx,
    pattern: &Pattern,
    scrut_ty: &RType,
    storage: &PatScrut,
    no_match_target: u32,
) -> Result<(), Error> {
    use crate::ast::PatternKind;
    let resolved_scrut = substitute_rtype(scrut_ty, &ctx.env);
    match &pattern.kind {
        PatternKind::Wildcard => Ok(()),
        PatternKind::Binding { name, by_ref, mutable, .. } => {
            if *by_ref {
                bind_pattern_ref(ctx, name, &resolved_scrut, storage, *mutable);
            } else {
                bind_pattern_value(ctx, name, &resolved_scrut, storage, pattern.id);
            }
            Ok(())
        }
        PatternKind::At { name, inner, .. } => {
            bind_pattern_value(ctx, name, &resolved_scrut, storage, pattern.id);
            codegen_pattern(ctx, inner, scrut_ty, storage, no_match_target)
        }
        PatternKind::LitInt(n) => {
            // Compare scrutinee value to literal. scrut_ty is some Int kind.
            push_scrut_value(ctx, &resolved_scrut, storage);
            push_int_lit(ctx, &resolved_scrut, *n);
            push_int_eq(ctx, &resolved_scrut);
            // Now the stack has an i32: 1 if equal, 0 otherwise.
            // i32.eqz → 1 if not equal; if so, br no_match.
            ctx.instructions.push(wasm::Instruction::I32Eqz);
            ctx.instructions
                .push(wasm::Instruction::BrIf(no_match_target));
            Ok(())
        }
        PatternKind::LitBool(b) => {
            push_scrut_value(ctx, &resolved_scrut, storage);
            ctx.instructions
                .push(wasm::Instruction::I32Const(if *b { 1 } else { 0 }));
            ctx.instructions.push(wasm::Instruction::I32Eq);
            ctx.instructions.push(wasm::Instruction::I32Eqz);
            ctx.instructions
                .push(wasm::Instruction::BrIf(no_match_target));
            Ok(())
        }
        PatternKind::Range { lo, hi } => {
            // (lo <= scrut) && (scrut <= hi). On false, br no_match.
            // For simplicity, decompose: br_if-on-not-greater-than.
            // First check scrut >= lo.
            push_scrut_value(ctx, &resolved_scrut, storage);
            push_int_lit(ctx, &resolved_scrut, *lo);
            push_int_ge(ctx, &resolved_scrut);
            ctx.instructions.push(wasm::Instruction::I32Eqz);
            ctx.instructions
                .push(wasm::Instruction::BrIf(no_match_target));
            // Then scrut <= hi.
            push_scrut_value(ctx, &resolved_scrut, storage);
            push_int_lit(ctx, &resolved_scrut, *hi);
            push_int_le(ctx, &resolved_scrut);
            ctx.instructions.push(wasm::Instruction::I32Eqz);
            ctx.instructions
                .push(wasm::Instruction::BrIf(no_match_target));
            Ok(())
        }
        PatternKind::Tuple(elems) => {
            // Recurse into each element's sub-storage.
            let elem_tys = match &resolved_scrut {
                RType::Tuple(es) => es.clone(),
                _ => unreachable!("typeck verified tuple pattern matches tuple type"),
            };
            // Sub-storage for each element.
            match storage {
                PatScrut::Locals { start } => {
                    let mut flat_off: u32 = 0;
                    let mut i = 0;
                    while i < elems.len() {
                        let sub_storage = PatScrut::Locals { start: start + flat_off };
                        codegen_pattern(ctx, &elems[i], &elem_tys[i], &sub_storage, no_match_target)?;
                        let mut sub_vts: Vec<wasm::ValType> = Vec::new();
                        crate::typeck::flatten_rtype(&elem_tys[i], ctx.structs, &mut sub_vts);
                        flat_off += sub_vts.len() as u32;
                        i += 1;
                    }
                }
                PatScrut::Memory { addr_local, byte_offset } => {
                    let mut byte_off = *byte_offset;
                    let mut i = 0;
                    while i < elems.len() {
                        let sub_storage = PatScrut::Memory {
                            addr_local: *addr_local,
                            byte_offset: byte_off,
                        };
                        codegen_pattern(ctx, &elems[i], &elem_tys[i], &sub_storage, no_match_target)?;
                        byte_off += byte_size_of(&elem_tys[i], ctx.structs, ctx.enums);
                        i += 1;
                    }
                }
            }
            Ok(())
        }
        PatternKind::Ref { inner, .. } => {
            // Scrutinee is `&T` whose flat is [I32]. The address it
            // holds points at the pointee. To match the inner pattern
            // against the pointee, switch to Memory storage rooted at
            // that address.
            let inner_ty = match &resolved_scrut {
                RType::Ref { inner, .. } => (**inner).clone(),
                _ => unreachable!("typeck verified ref pattern matches ref type"),
            };
            // Push the address scalar from storage and stash to a fresh local.
            push_scrut_value(ctx, &resolved_scrut, storage);
            let addr_local = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions
                .push(wasm::Instruction::LocalSet(addr_local));
            let inner_storage = PatScrut::Memory { addr_local, byte_offset: 0 };
            codegen_pattern(ctx, inner, &inner_ty, &inner_storage, no_match_target)
        }
        PatternKind::VariantTuple { path, elems } => {
            codegen_variant_pattern(
                ctx,
                path,
                Some(elems),
                None,
                false,
                &resolved_scrut,
                storage,
                no_match_target,
            )
        }
        PatternKind::VariantStruct { path, fields, rest } => {
            // Same syntactic shape covers both enum struct-variants
            // (`E::V { f: e }`) and bare struct destructure
            // (`Point { x, y }`). Dispatch on the resolved scrutinee
            // type — variant patterns must hit Memory storage and
            // need a disc check; struct patterns just walk fields.
            if matches!(&resolved_scrut, RType::Struct { .. }) {
                codegen_struct_pattern(
                    ctx,
                    fields,
                    *rest,
                    &resolved_scrut,
                    storage,
                    no_match_target,
                )
            } else {
                codegen_variant_pattern(
                    ctx,
                    path,
                    None,
                    Some(fields),
                    *rest,
                    &resolved_scrut,
                    storage,
                    no_match_target,
                )
            }
        }
        PatternKind::Or(alts) => {
            // Or-patterns: try each alternative in turn. We wrap each
            // alt in its own block; on match, we br PAST the entire
            // or-construct (jumping to the success path of the parent
            // pattern). On no-match for an alt, we fall through to the
            // next alt. If the last alt fails, we br to the parent's
            // no_match.
            //
            // Implementation: outer block "or_match"; for each alt
            // except last, an inner block "alt_no_match"; on alt match,
            // br to or_match. On alt no-match, fall through. Last alt
            // uses the parent's no_match_target directly.
            //
            // After the or-construct: control proceeds normally
            // (matched). Nested wasm `block`s account for the increased
            // br-depth so that the parent's no_match_target is
            // referenced as `no_match_target + nesting_depth`.
            if alts.is_empty() {
                return Ok(());
            }
            ctx.instructions
                .push(wasm::Instruction::Block(wasm::BlockType::Empty));
            // Inside or_match block, parent's no_match is at depth
            // no_match_target + 1.
            let parent_no_match = no_match_target + 1;
            let mut k = 0;
            while k + 1 < alts.len() {
                ctx.instructions
                    .push(wasm::Instruction::Block(wasm::BlockType::Empty));
                // Within this inner block, no-match within the alt
                // means br 0 (this block's end → fall through to next alt).
                codegen_pattern(ctx, &alts[k], scrut_ty, storage, 0)?;
                // Match: br 1 to escape both this inner and continue
                // past the or_match block.
                ctx.instructions.push(wasm::Instruction::Br(1));
                ctx.instructions.push(wasm::Instruction::End);
                k += 1;
            }
            // Last alt: no fallback; failure goes straight to parent.
            codegen_pattern(ctx, &alts[k], scrut_ty, storage, parent_no_match)?;
            ctx.instructions.push(wasm::Instruction::End);
            Ok(())
        }
    }
}

// Compute the address `addr_local + byte_offset` and load the
// scrutinee's flat scalars onto the wasm stack. For Locals storage,
// just `local.get` each slot. For Memory storage, `iN.load` each leaf
// from the computed address.
fn push_scrut_value(ctx: &mut FnCtx, ty: &RType, storage: &PatScrut) {
    match storage {
        PatScrut::Locals { start } => {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
            let mut k = 0;
            while k < vts.len() {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(start + k as u32));
                k += 1;
            }
        }
        PatScrut::Memory { addr_local, byte_offset } => {
            let mut leaves: Vec<MemLeaf> = Vec::new();
            collect_leaves(ty, ctx.structs, ctx.enums, *byte_offset, &mut leaves);
            let mut k = 0;
            while k < leaves.len() {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(*addr_local));
                ctx.instructions.push(load_instr(&leaves[k], 0));
                k += 1;
            }
        }
    }
}

// Push an integer literal onto the stack with the right wasm value type
// for the target int kind. For 128-bit kinds the literal must be ≤
// i64::MAX (typeck range-checks pattern literals against the kind).
fn push_int_lit(ctx: &mut FnCtx, ty: &RType, n: u64) {
    match ty {
        RType::Int(k) => {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
            match (vts.len(), &vts[0]) {
                (1, wasm::ValType::I32) => {
                    ctx.instructions
                        .push(wasm::Instruction::I32Const(n as i32));
                }
                (1, wasm::ValType::I64) => {
                    ctx.instructions
                        .push(wasm::Instruction::I64Const(n as i64));
                }
                (2, _) => {
                    // 128-bit: pattern matching not yet supported
                    // because an Eq comparison needs 128-bit cmp. Reject.
                    let _ = k;
                    ctx.instructions
                        .push(wasm::Instruction::I64Const(n as i64));
                    ctx.instructions
                        .push(wasm::Instruction::I64Const(0));
                }
                _ => unreachable!(),
            }
        }
        _ => unreachable!("push_int_lit on non-Int"),
    }
}

// Push the equality test result (i32: 1 if equal, 0 otherwise) for two
// values of `ty` on the wasm stack. For Int types: iN.eq on the single
// scalar (or a 128-bit decomposition for u128/i128). For bool: i32.eq.
fn push_int_eq(ctx: &mut FnCtx, ty: &RType) {
    match ty {
        RType::Int(k) => {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
            match (vts.len(), &vts[0]) {
                (1, wasm::ValType::I32) => {
                    ctx.instructions.push(wasm::Instruction::I32Eq);
                }
                (1, wasm::ValType::I64) => {
                    ctx.instructions.push(wasm::Instruction::I64Eq);
                }
                _ => {
                    // 128-bit equality: stack has (a_lo, a_hi, b_lo, b_hi).
                    // For simplicity, do a placeholder: replace with proper
                    // 128-bit eq when needed. For now a low-only check.
                    let _ = k;
                    // Pop b_hi and a_hi (drop), then compare lows.
                    ctx.instructions.push(wasm::Instruction::Drop);
                    let tmp = ctx.next_wasm_local;
                    ctx.extra_locals.push(wasm::ValType::I64);
                    ctx.next_wasm_local += 1;
                    ctx.instructions.push(wasm::Instruction::LocalSet(tmp));
                    ctx.instructions.push(wasm::Instruction::Drop);
                    ctx.instructions.push(wasm::Instruction::LocalGet(tmp));
                    ctx.instructions.push(wasm::Instruction::I64Eq);
                }
            }
        }
        RType::Bool => {
            ctx.instructions.push(wasm::Instruction::I32Eq);
        }
        _ => unreachable!("push_int_eq on non-Int/Bool"),
    }
}

// Push 1 if the second operand is `>=` the first, 0 otherwise. Picks
// signed/unsigned variant from the int's signedness.
fn push_int_ge(ctx: &mut FnCtx, ty: &RType) {
    match ty {
        RType::Int(k) => {
            let signed = int_kind_is_signed(k);
            let mut vts: Vec<wasm::ValType> = Vec::new();
            crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
            match (vts.len(), &vts[0]) {
                (1, wasm::ValType::I32) => {
                    if signed {
                        ctx.instructions.push(wasm::Instruction::I32GeS);
                    } else {
                        ctx.instructions.push(wasm::Instruction::I32GeU);
                    }
                }
                (1, wasm::ValType::I64) => {
                    if signed {
                        ctx.instructions.push(wasm::Instruction::I64GeS);
                    } else {
                        ctx.instructions.push(wasm::Instruction::I64GeU);
                    }
                }
                _ => {
                    // 128-bit: not yet
                    ctx.instructions.push(wasm::Instruction::I32Const(1));
                }
            }
        }
        _ => unreachable!("push_int_ge on non-Int"),
    }
}

fn push_int_le(ctx: &mut FnCtx, ty: &RType) {
    match ty {
        RType::Int(k) => {
            let signed = int_kind_is_signed(k);
            let mut vts: Vec<wasm::ValType> = Vec::new();
            crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
            match (vts.len(), &vts[0]) {
                (1, wasm::ValType::I32) => {
                    if signed {
                        ctx.instructions.push(wasm::Instruction::I32LeS);
                    } else {
                        ctx.instructions.push(wasm::Instruction::I32LeU);
                    }
                }
                (1, wasm::ValType::I64) => {
                    if signed {
                        ctx.instructions.push(wasm::Instruction::I64LeS);
                    } else {
                        ctx.instructions.push(wasm::Instruction::I64LeU);
                    }
                }
                _ => {
                    ctx.instructions.push(wasm::Instruction::I32Const(1));
                }
            }
        }
        _ => unreachable!("push_int_le on non-Int"),
    }
}

fn int_kind_is_signed(k: &crate::typeck::IntKind) -> bool {
    use crate::typeck::IntKind;
    matches!(
        k,
        IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64 | IntKind::I128 | IntKind::Isize
    )
}

// Bind a pattern's name to the matched value at `storage`. The binding
// is added to `ctx.locals` so that `Var(name)` inside the arm body
// finds it.
//
// For non-enum types: the binding owns its own fresh wasm-local range
// holding the value's flat scalars. We copy from the scrutinee
// storage (locals or memory) into those slots.
//
// For enum types: the binding's storage is an address (the binding's
// flat representation is [I32]). When the scrutinee is at Memory, the
// address `addr + byte_offset` IS the binding's address; we cache it
// in a fresh i32 local.
// Bind by reference: `ref name` (or `ref mut name`). The binding's
// type is `&T` (or `&mut T`), and its storage is a single i32 wasm
// local holding the address of the matched place — `addr_local +
// byte_offset` for Memory storage. The Locals case never reaches
// here: codegen_match_expr pre-walks the patterns and, if any arm
// uses `ref`, spills the scrutinee to the shadow stack so storage
// is always Memory by the time we get here.
fn bind_pattern_ref(
    ctx: &mut FnCtx,
    name: &str,
    ty: &RType,
    storage: &PatScrut,
    mutable: bool,
) {
    let ref_ty = RType::Ref {
        inner: Box::new(ty.clone()),
        mutable,
        lifetime: crate::typeck::LifetimeRepr::Inferred(0),
    };
    let dest = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    match storage {
        PatScrut::Memory { addr_local, byte_offset } => {
            ctx.instructions
                .push(wasm::Instruction::LocalGet(*addr_local));
            if *byte_offset != 0 {
                ctx.instructions
                    .push(wasm::Instruction::I32Const(*byte_offset as i32));
                ctx.instructions.push(wasm::Instruction::I32Add);
            }
            ctx.instructions.push(wasm::Instruction::LocalSet(dest));
        }
        PatScrut::Locals { .. } => {
            unreachable!(
                "ref binding against locals storage — codegen_match_expr should have spilled"
            )
        }
    }
    ctx.locals.push(LocalBinding {
        name: name.to_string(),
        rtype: ref_ty,
        storage: Storage::Local {
            wasm_start: dest,
            flat_size: 1,
        },
    });
}

fn bind_pattern_value(
    ctx: &mut FnCtx,
    name: &str,
    ty: &RType,
    storage: &PatScrut,
    pattern_id: crate::ast::NodeId,
) {
    let is_enum = matches!(ty, RType::Enum { .. });
    if is_enum {
        // Bind = the address of this enum value.
        let dest = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        match storage {
            PatScrut::Locals { start } => {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(*start));
                ctx.instructions.push(wasm::Instruction::LocalSet(dest));
            }
            PatScrut::Memory { addr_local, byte_offset } => {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(*addr_local));
                if *byte_offset != 0 {
                    ctx.instructions
                        .push(wasm::Instruction::I32Const(*byte_offset as i32));
                    ctx.instructions.push(wasm::Instruction::I32Add);
                }
                ctx.instructions.push(wasm::Instruction::LocalSet(dest));
            }
        }
        ctx.locals.push(LocalBinding {
            name: name.to_string(),
            rtype: ty.clone(),
            storage: Storage::Local {
                wasm_start: dest,
                flat_size: 1,
            },
        });
        return;
    }
    // Non-enum value: if escape analysis flagged this binding as
    // addressed, allocate a shadow-stack slot up front so reads /
    // writes / borrows all share one stable location. Otherwise stash
    // into wasm locals (the fast path).
    let addressed = (pattern_id as usize) < ctx.pattern_addressed.len()
        && ctx.pattern_addressed[pattern_id as usize];
    if addressed {
        let bytes = byte_size_of(ty, ctx.structs, ctx.enums);
        ctx.instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions
            .push(wasm::Instruction::I32Const(bytes as i32));
        ctx.instructions.push(wasm::Instruction::I32Sub);
        ctx.instructions
            .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
        let addr_local = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.instructions
            .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
        // Copy from scrutinee storage to addr_local.
        match storage {
            PatScrut::Locals { start } => {
                let mut vts: Vec<wasm::ValType> = Vec::new();
                crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
                let mut k = 0;
                while k < vts.len() {
                    ctx.instructions
                        .push(wasm::Instruction::LocalGet(start + k as u32));
                    k += 1;
                }
                store_flat_to_memory(ctx, ty, BaseAddr::WasmLocal(addr_local), 0);
            }
            PatScrut::Memory { addr_local: src_addr, byte_offset } => {
                let src = *src_addr;
                let off = *byte_offset;
                if off != 0 {
                    let src2 = ctx.next_wasm_local;
                    ctx.extra_locals.push(wasm::ValType::I32);
                    ctx.next_wasm_local += 1;
                    ctx.instructions.push(wasm::Instruction::LocalGet(src));
                    ctx.instructions.push(wasm::Instruction::I32Const(off as i32));
                    ctx.instructions.push(wasm::Instruction::I32Add);
                    ctx.instructions.push(wasm::Instruction::LocalSet(src2));
                    emit_memcpy(ctx, addr_local, src2, bytes);
                } else {
                    emit_memcpy(ctx, addr_local, src, bytes);
                }
            }
        }
        ctx.locals.push(LocalBinding {
            name: name.to_string(),
            rtype: ty.clone(),
            storage: Storage::MemoryAt { addr_local },
        });
        return;
    }
    let mut vts: Vec<wasm::ValType> = Vec::new();
    crate::typeck::flatten_rtype(ty, ctx.structs, &mut vts);
    let dest_start = ctx.next_wasm_local;
    let mut k = 0;
    while k < vts.len() {
        ctx.extra_locals.push(vts[k]);
        ctx.next_wasm_local += 1;
        k += 1;
    }
    match storage {
        PatScrut::Locals { start } => {
            let mut k = 0;
            while k < vts.len() {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(start + k as u32));
                ctx.instructions
                    .push(wasm::Instruction::LocalSet(dest_start + k as u32));
                k += 1;
            }
        }
        PatScrut::Memory { addr_local, byte_offset } => {
            // Load each leaf from memory into the binding's locals.
            let mut leaves: Vec<MemLeaf> = Vec::new();
            collect_leaves(ty, ctx.structs, ctx.enums, *byte_offset, &mut leaves);
            // Push all leaves onto the wasm stack first (in declaration
            // order), then pop into locals in reverse so each leaf
            // lands in its corresponding local slot.
            let mut k = 0;
            while k < leaves.len() {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(*addr_local));
                ctx.instructions.push(load_instr(&leaves[k], 0));
                k += 1;
            }
            let mut k = leaves.len();
            while k > 0 {
                k -= 1;
                ctx.instructions
                    .push(wasm::Instruction::LocalSet(dest_start + k as u32));
            }
        }
    }
    ctx.locals.push(LocalBinding {
        name: name.to_string(),
        rtype: ty.clone(),
        storage: Storage::Local {
            wasm_start: dest_start,
            flat_size: vts.len() as u32,
        },
    });
}

// Variant pattern check + binding. Branches to `no_match_target` if the
// scrutinee's discriminant doesn't match the variant's, or if any
// payload sub-pattern fails.
// `Point { x, y }` against a struct scrutinee. No disc check (it's a
// product, not a sum); just recurse into each named field's
// sub-storage. The scrutinee storage may be Locals or Memory; we
// recurse with the corresponding sub-range / sub-offset per field
// in declaration order.
fn codegen_struct_pattern(
    ctx: &mut FnCtx,
    fields: &Vec<crate::ast::FieldPattern>,
    _rest: bool,
    scrut_ty: &RType,
    storage: &PatScrut,
    no_match_target: u32,
) -> Result<(), Error> {
    let (struct_path, struct_args) = match scrut_ty {
        RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
        _ => unreachable!("struct pattern requires struct scrutinee"),
    };
    let entry =
        struct_lookup(ctx.structs, &struct_path).expect("typeck verified struct exists");
    let env = make_struct_env(&entry.type_params, &struct_args);
    // Build (name, byte_offset, flat_offset, ty) for each declared field.
    struct FieldInfo {
        name: String,
        byte_offset: u32,
        flat_offset: u32,
        ty: RType,
    }
    let mut infos: Vec<FieldInfo> = Vec::with_capacity(entry.fields.len());
    let mut byte_off: u32 = 0;
    let mut flat_off: u32 = 0;
    let mut k = 0;
    while k < entry.fields.len() {
        let fty = substitute_rtype(&entry.fields[k].ty, &env);
        let mut vts: Vec<wasm::ValType> = Vec::new();
        crate::typeck::flatten_rtype(&fty, ctx.structs, &mut vts);
        let fb = byte_size_of(&fty, ctx.structs, ctx.enums);
        infos.push(FieldInfo {
            name: entry.fields[k].name.clone(),
            byte_offset: byte_off,
            flat_offset: flat_off,
            ty: fty,
        });
        byte_off += fb;
        flat_off += vts.len() as u32;
        k += 1;
    }
    let mut k = 0;
    while k < fields.len() {
        let fp = &fields[k];
        let mut idx: Option<usize> = None;
        let mut j = 0;
        while j < infos.len() {
            if infos[j].name == fp.name {
                idx = Some(j);
                break;
            }
            j += 1;
        }
        let i = idx.expect("typeck verified field name");
        let sub_storage = match storage {
            PatScrut::Memory { addr_local, byte_offset } => PatScrut::Memory {
                addr_local: *addr_local,
                byte_offset: *byte_offset + infos[i].byte_offset,
            },
            PatScrut::Locals { start } => PatScrut::Locals {
                start: *start + infos[i].flat_offset,
            },
        };
        codegen_pattern(ctx, &fp.pattern, &infos[i].ty, &sub_storage, no_match_target)?;
        k += 1;
    }
    Ok(())
}

fn codegen_variant_pattern(
    ctx: &mut FnCtx,
    path: &Path,
    tuple_elems: Option<&Vec<Pattern>>,
    struct_fields: Option<&Vec<crate::ast::FieldPattern>>,
    _rest: bool,
    scrut_ty: &RType,
    storage: &PatScrut,
    no_match_target: u32,
) -> Result<(), Error> {
    // Resolve the variant by name (last segment) against the scrutinee's
    // enum type. We can rely on the scrutinee being the right enum type
    // because typeck already unified the pattern path to the scrutinee's
    // enum. This means a name lookup against the scrutinee's variants
    // is sufficient.
    let (enum_path, enum_type_args) = match scrut_ty {
        RType::Enum { path, type_args, .. } => (path.clone(), type_args.clone()),
        _ => unreachable!("typeck verified variant pattern against enum"),
    };
    let entry =
        crate::typeck::enum_lookup(ctx.enums, &enum_path).expect("typeck verified enum exists");
    let variant_name = path
        .segments
        .last()
        .map(|s| s.name.clone())
        .expect("path has at least one segment");
    let mut variant_idx: Option<usize> = None;
    let mut k = 0;
    while k < entry.variants.len() {
        if entry.variants[k].name == variant_name {
            variant_idx = Some(k);
            break;
        }
        k += 1;
    }
    let variant_idx = variant_idx.expect("typeck verified variant name");
    let disc = entry.variants[variant_idx].disc;
    // Read disc from scrutinee's address. Variant patterns always
    // require Memory storage (enums are address-passed). Compare to
    // the variant's disc; on mismatch, br no_match.
    let (addr_local, byte_offset) = match storage {
        PatScrut::Memory { addr_local, byte_offset } => (*addr_local, *byte_offset),
        _ => unreachable!("variant pattern scrutinee must be Memory storage"),
    };
    ctx.instructions
        .push(wasm::Instruction::LocalGet(addr_local));
    ctx.instructions.push(wasm::Instruction::I32Load {
        align: 2,
        offset: byte_offset,
    });
    ctx.instructions
        .push(wasm::Instruction::I32Const(disc as i32));
    ctx.instructions.push(wasm::Instruction::I32Ne);
    ctx.instructions
        .push(wasm::Instruction::BrIf(no_match_target));
    // Recurse into payload sub-patterns.
    let env = build_env(&entry.type_params, &enum_type_args);
    let payload_byte_base = byte_offset + 4;
    match (&entry.variants[variant_idx].payload, tuple_elems, struct_fields) {
        (crate::typeck::VariantPayloadResolved::Unit, _, _) => Ok(()),
        (crate::typeck::VariantPayloadResolved::Tuple(types), Some(elems), _) => {
            let mut byte_off = payload_byte_base;
            let mut i = 0;
            while i < elems.len() {
                let elem_ty = substitute_rtype(&types[i], &env);
                let sub_storage = PatScrut::Memory {
                    addr_local,
                    byte_offset: byte_off,
                };
                codegen_pattern(ctx, &elems[i], &elem_ty, &sub_storage, no_match_target)?;
                byte_off += byte_size_of(&elem_ty, ctx.structs, ctx.enums);
                i += 1;
            }
            Ok(())
        }
        (crate::typeck::VariantPayloadResolved::Struct(field_defs), _, Some(field_pats)) => {
            // Build (name → byte_offset, sub_ty) for declared fields.
            let mut decl_offsets: Vec<u32> = Vec::with_capacity(field_defs.len());
            let mut decl_subst_tys: Vec<RType> = Vec::with_capacity(field_defs.len());
            let mut byte_off = payload_byte_base;
            let mut k = 0;
            while k < field_defs.len() {
                let fty = substitute_rtype(&field_defs[k].ty, &env);
                decl_offsets.push(byte_off);
                byte_off += byte_size_of(&fty, ctx.structs, ctx.enums);
                decl_subst_tys.push(fty);
                k += 1;
            }
            let mut k = 0;
            while k < field_pats.len() {
                let fp = &field_pats[k];
                let mut decl_idx: Option<usize> = None;
                let mut j = 0;
                while j < field_defs.len() {
                    if field_defs[j].name == fp.name {
                        decl_idx = Some(j);
                        break;
                    }
                    j += 1;
                }
                let idx = decl_idx.expect("typeck verified field name");
                let sub_storage = PatScrut::Memory {
                    addr_local,
                    byte_offset: decl_offsets[idx],
                };
                codegen_pattern(ctx, &fp.pattern, &decl_subst_tys[idx], &sub_storage, no_match_target)?;
                k += 1;
            }
            Ok(())
        }
        _ => unreachable!("typeck rejected mismatched variant pattern shape"),
    }
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
            ctx.method_resolutions[res_idx].as_ref().unwrap().type_args.clone();
        let concrete = subst_vec(&raw_args, &ctx.env);
        let return_rt = {
            let tmpl = &ctx.funcs.templates[template_idx];
            let tmpl_env = build_env(&tmpl.type_params, &concrete);
            match &tmpl.return_type {
                Some(rt) => substitute_rtype(rt, &tmpl_env),
                None => RType::Tuple(Vec::new()),
            }
        };
        let idx = ctx.mono.intern(template_idx, concrete);
        (idx, return_rt)
    } else {
        let callee_idx = ctx.method_resolutions[res_idx].as_ref().unwrap().callee_idx;
        let return_rt = {
            let entry = &ctx.funcs.entries[callee_idx_to_table_idx(ctx, callee_idx)];
            match &entry.return_type {
                Some(rt) => rt.clone(),
                None => RType::Tuple(Vec::new()),
            }
        };
        (callee_idx, return_rt)
    };
    // Enum-returning callees use sret: leading i32 is a caller-
    // allocated destination slot, written by the callee before return
    // and surfaced as the call's wasm result. Allocate it from the
    // caller's shadow stack before pushing the receiver/args.
    let returns_enum = matches!(&return_rt, RType::Enum { .. });
    if returns_enum {
        let bytes = byte_size_of(&return_rt, ctx.structs, ctx.enums);
        ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
        ctx.instructions.push(wasm::Instruction::I32Sub);
        ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    }
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
            trait_path: t.trait_path.clone(),
            trait_args: t.trait_args.clone(),
            method_name: t.method_name.clone(),
            recv_type: t.recv_type.clone(),
        })
        .unwrap();
    // Already substituted at the time of mono cloning, but still need to
    // peel any `Ref` wrapper if the recv type was symbolic ref.
    let concrete_recv = match &td.recv_type {
        RType::Ref { inner, .. } => (**inner).clone(),
        other => other.clone(),
    };
    let resolution = match crate::typeck::solve_impl_with_args(&td.trait_path, &td.trait_args, &concrete_recv, ctx.traits, 0)
    {
        Some(r) => r,
        None => unreachable!(
            "no impl of `{}` for `{}` at mono time — typeck should have caught",
            crate::typeck::place_to_string(&td.trait_path),
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
                Some(rt) => rt.clone(),
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
                        found = Some(resolution.subst[j].1.clone());
                        break;
                    }
                    j += 1;
                }
                concrete.push(found.expect("impl-param not bound by subst"));
                k += 1;
            }
            let method_param_count = tmpl.type_params.len() - impl_param_count;
            let recorded_type_args =
                ctx.method_resolutions[res_idx].as_ref().unwrap().type_args.clone();
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
    // Sret handling for enum-returning trait methods: same convention
    // as `codegen_call` / `codegen_method_call` — caller allocates the
    // dest slot and pushes its address as the leading param.
    let returns_enum = matches!(&return_rt, RType::Enum { .. });
    if returns_enum {
        let bytes = byte_size_of(&return_rt, ctx.structs, ctx.enums);
        ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
        ctx.instructions.push(wasm::Instruction::I32Sub);
        ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    }
    // Codegen receiver per the recorded recv_adjust (derived from the
    // trait method's declared receiver shape during typeck).
    let recv_adjust = ctx.method_resolutions[res_idx]
        .as_ref()
        .unwrap()
        .recv_adjust;
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

// Numeric literal codegen. With literal overloading dropped, every
// integer literal resolves to a built-in `Int(kind)`; emit the
// appropriate `iN.const`. 64-bit kinds (u64/i64) take an `i64.const`;
// 128-bit kinds need two `i64.const`s (low half then high half) per
// the wide flatten convention. ≤32-bit kinds (incl. usize/isize on
// our wasm32 target) use `i32.const`. `negative` flips the sign for
// `NegIntLit(N)` source forms; the typeck range-check already
// rejected unsigned/out-of-range signed targets.
fn emit_int_lit(ctx: &mut FnCtx, ty: &RType, value: u64, negative: bool) {
    let kind = match ty {
        RType::Int(k) => k.clone(),
        _ => unreachable!(
            "literal target must be Int after typeck — got {}",
            crate::typeck::rtype_to_string(ty)
        ),
    };
    let signed64 = if negative {
        (value as i64).wrapping_neg()
    } else {
        value as i64
    };
    match int_kind_class(&kind) {
        IntClass::Wide128 => {
            // Two halves: low (signed64) then high (sign-extension or 0).
            let high = if int_kind_signed(&kind) && negative { -1i64 } else { 0i64 };
            ctx.instructions.push(wasm::Instruction::I64Const(signed64));
            ctx.instructions.push(wasm::Instruction::I64Const(high));
        }
        IntClass::Wide64 => {
            ctx.instructions.push(wasm::Instruction::I64Const(signed64));
        }
        IntClass::Narrow32 => {
            ctx.instructions
                .push(wasm::Instruction::I32Const(signed64 as i32));
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
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    let result_ty = match &block.tail {
        Some(expr) => codegen_expr(ctx, expr)?,
        // No tail ⇒ block evaluates to `()` (the empty tuple). Nothing
        // to push on the wasm stack; nothing to save across the drop
        // sequence below.
        None => RType::Tuple(Vec::new()),
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
            let rt = ctx.locals[i].rtype.clone();
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
                Storage::MemoryAt { addr_local } => {
                    load_flat_from_memory(ctx, &rt, BaseAddr::WasmLocal(*addr_local), 0);
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
    // Snapshot the resolution upfront so we don't keep a borrow of ctx.call_resolutions.
    let resolution = ctx.call_resolutions[res_idx]
        .as_ref()
        .expect("typeck registered this call");
    let snapshot = match resolution {
        CallResolution::Direct(idx) => CallResolution::Direct(*idx),
        CallResolution::Generic { template_idx, type_args } => CallResolution::Generic {
            template_idx: *template_idx,
            type_args: type_args.clone(),
        },
        CallResolution::Variant { enum_path, disc, type_args } => CallResolution::Variant {
            enum_path: enum_path.clone(),
            disc: *disc,
            type_args: type_args.clone(),
        },
    };
    if let CallResolution::Variant { enum_path, disc, type_args } = &snapshot {
        return codegen_variant_construction(
            ctx,
            &call.args,
            enum_path,
            *disc,
            type_args,
            None,
        );
    }
    let (func_idx, return_rt) = match &snapshot {
        CallResolution::Direct(idx) => {
            let entry = &ctx.funcs.entries[*idx];
            let rt = match &entry.return_type {
                Some(rt) => rt.clone(),
                None => RType::Tuple(Vec::new()),
            };
            (entry.idx, rt)
        }
        CallResolution::Generic { template_idx, type_args } => {
            let concrete = subst_vec(type_args, &ctx.env);
            let template_idx_copy = *template_idx;
            let return_rt = {
                let tmpl: &GenericTemplate = &ctx.funcs.templates[template_idx_copy];
                let tmpl_env = build_env(&tmpl.type_params, &concrete);
                match &tmpl.return_type {
                    Some(rt) => substitute_rtype(rt, &tmpl_env),
                    None => RType::Tuple(Vec::new()),
                }
            };
            let idx = ctx.mono.intern(template_idx_copy, concrete);
            (idx, return_rt)
        }
        CallResolution::Variant { .. } => unreachable!("variant case handled above"),
    };
    // Functions returning an enum use sret: the caller allocates a slot
    // in its own frame for the return value and passes its address as
    // a leading i32 param. The function writes there before returning.
    let returns_enum = matches!(&return_rt, RType::Enum { .. });
    if returns_enum {
        let bytes = byte_size_of(&return_rt, ctx.structs, ctx.enums);
        ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
        ctx.instructions.push(wasm::Instruction::I32Sub);
        ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
        ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    }
    let mut i = 0;
    while i < call.args.len() {
        codegen_expr(ctx, &call.args[i])?;
        i += 1;
    }
    ctx.instructions.push(wasm::Instruction::Call(func_idx));
    Ok(return_rt)
}

// Lower variant construction (`E::Variant(args)` for tuple variants or
// `E::Variant { f: e }` for struct variants — same shape from codegen's
// view: a list of payload expressions, each with a known offset within
// the variant's payload). `field_names` is `Some(names)` for struct
// variants (used to map FieldInits to declared field positions) or
// `None` for tuple variants (positional).
//
// Layout: i32 disc at offset 0 (always), then payload bytes starting
// at offset 4 in declaration order. Smaller variants leave the trailing
// bytes of the enum's max-payload buffer unused.
//
// Returns RType::Enum (with concrete type_args). The wasm stack ends
// with the address (i32) of the freshly allocated slot.
fn codegen_variant_construction(
    ctx: &mut FnCtx,
    payload_exprs: &Vec<Expr>,
    enum_path: &Vec<String>,
    disc: u32,
    type_args: &Vec<RType>,
    field_names: Option<&Vec<String>>,
) -> Result<RType, Error> {
    let concrete_type_args = subst_vec(type_args, &ctx.env);
    let enum_ty = RType::Enum {
        path: enum_path.clone(),
        type_args: concrete_type_args.clone(),
        lifetime_args: Vec::new(),
    };
    let total_size = byte_size_of(&enum_ty, ctx.structs, ctx.enums);
    // Payload type list, substituted under the enum's type-args env.
    let entry = crate::typeck::enum_lookup(ctx.enums, enum_path)
        .expect("typeck verified the enum exists");
    let env = build_env(&entry.type_params, &concrete_type_args);
    let variant = &entry.variants[disc as usize];
    // Build (declared_offset, payload_type) pairs in payload-declaration
    // order. For struct variants, also build a name → declared-position
    // mapping so we can route FieldInit by name.
    let (payload_offsets, payload_types, struct_field_names): (Vec<u32>, Vec<RType>, Vec<String>) = {
        let mut offsets: Vec<u32> = Vec::new();
        let mut types: Vec<RType> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut off: u32 = 4; // disc takes the first 4 bytes
        match &variant.payload {
            crate::typeck::VariantPayloadResolved::Unit => {}
            crate::typeck::VariantPayloadResolved::Tuple(types_decl) => {
                let mut i = 0;
                while i < types_decl.len() {
                    let ty = substitute_rtype(&types_decl[i], &env);
                    offsets.push(off);
                    off += byte_size_of(&ty, ctx.structs, ctx.enums);
                    types.push(ty);
                    i += 1;
                }
            }
            crate::typeck::VariantPayloadResolved::Struct(fields) => {
                let mut i = 0;
                while i < fields.len() {
                    let ty = substitute_rtype(&fields[i].ty, &env);
                    offsets.push(off);
                    off += byte_size_of(&ty, ctx.structs, ctx.enums);
                    types.push(ty);
                    names.push(fields[i].name.clone());
                    i += 1;
                }
            }
        }
        (offsets, types, names)
    };
    // Allocate the slot: __sp -= total_size; the new __sp is the address.
    ctx.instructions
        .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions
        .push(wasm::Instruction::I32Const(total_size as i32));
    ctx.instructions.push(wasm::Instruction::I32Sub);
    ctx.instructions
        .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    // Cache the address in a wasm local so we can reuse it across
    // the disc + per-field stores.
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions
        .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions
        .push(wasm::Instruction::LocalSet(addr_local));
    // Store the discriminant at offset 0.
    ctx.instructions
        .push(wasm::Instruction::LocalGet(addr_local));
    ctx.instructions
        .push(wasm::Instruction::I32Const(disc as i32));
    ctx.instructions.push(wasm::Instruction::I32Store {
        align: 2,
        offset: 0,
    });
    // Map each payload_expr to its declared position. For tuple variants
    // payload_exprs is in declaration order. For struct variants we use
    // field_names to find each FieldInit's position.
    if payload_exprs.len() != payload_offsets.len() {
        unreachable!("typeck verified payload arity");
    }
    let mut decl_pos_for_arg: Vec<usize> = Vec::with_capacity(payload_exprs.len());
    if let Some(names) = field_names {
        // Struct variant: each payload_exprs[i] corresponds to names[i]
        // (the order the user wrote them). Map to declared position.
        let mut i = 0;
        while i < names.len() {
            let mut found: Option<usize> = None;
            let mut k = 0;
            while k < struct_field_names.len() {
                if struct_field_names[k] == names[i] {
                    found = Some(k);
                    break;
                }
                k += 1;
            }
            decl_pos_for_arg.push(found.expect("typeck verified field name"));
            i += 1;
        }
    } else {
        // Tuple variant: positional.
        let mut i = 0;
        while i < payload_exprs.len() {
            decl_pos_for_arg.push(i);
            i += 1;
        }
    }
    // For each argument: load address, codegen value (pushes flat scalars),
    // then store-flat-to-memory at the field's offset. We use the existing
    // `store_flat_to_memory` machinery so multi-scalar fields (u128, structs)
    // get all their bytes written at the right offsets.
    let mut i = 0;
    while i < payload_exprs.len() {
        let decl_pos = decl_pos_for_arg[i];
        let off = payload_offsets[decl_pos];
        let ty = payload_types[decl_pos].clone();
        // codegen the value — pushes its flat scalars onto the wasm stack.
        codegen_expr(ctx, &payload_exprs[i])?;
        // store_flat_to_memory pops the flat scalars and stores them at
        // base + base_offset. Use the cached addr_local as the base.
        store_flat_to_memory(ctx, &ty, BaseAddr::WasmLocal(addr_local), off);
        i += 1;
    }
    // Push the address as the result of the construction expression.
    ctx.instructions
        .push(wasm::Instruction::LocalGet(addr_local));
    Ok(enum_ty)
}

fn codegen_struct_lit(
    ctx: &mut FnCtx,
    lit: &StructLit,
    node_id: crate::ast::NodeId,
) -> Result<RType, Error> {
    // Struct-variant construction routes through codegen_struct_lit but
    // the resolution recorded by typeck distinguishes it. Lower via the
    // shared variant-construction path; FieldInits map to declared
    // payload fields by name.
    if let Some(CallResolution::Variant { enum_path, disc, type_args }) =
        ctx.call_resolutions[node_id as usize].as_ref()
    {
        let enum_path = enum_path.clone();
        let disc = *disc;
        let type_args = type_args.clone();
        let mut payload_exprs: Vec<Expr> = Vec::new();
        let mut field_names: Vec<String> = Vec::new();
        let mut i = 0;
        while i < lit.fields.len() {
            field_names.push(lit.fields[i].name.clone());
            payload_exprs.push(lit.fields[i].value.clone());
            i += 1;
        }
        return codegen_variant_construction(
            ctx,
            &payload_exprs,
            &enum_path,
            disc,
            &type_args,
            Some(&field_names),
        );
    }
    // Read the resolved struct type recorded by typeck at this NodeId.
    // For generic structs, this carries the concrete type_args needed for
    // layout. Substitute under our env in case those args themselves reference
    // outer Param entries (mono of mono).
    let recorded_ty = ctx.expr_types[node_id as usize]
        .as_ref()
        .expect("typeck recorded this struct lit's type")
        .clone();
    let recorded_ty = substitute_rtype(&recorded_ty, &ctx.env);
    let (full, struct_args) = match &recorded_ty {
        RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
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
        ExprKind::TupleIndex { base, index, .. } => {
            out.push(format!("{}", index));
            collect_place_chain(base, out)
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
    let root_ty = ctx.locals[binding_idx].rtype.clone();
    let through_ref = matches!(&root_ty, RType::Ref { .. });

    // Walk chain to compute byte offset + final type.
    let mut current_ty = if through_ref {
        match &root_ty {
            RType::Ref { inner, .. } => (**inner).clone(),
            _ => unreachable!(),
        }
    } else {
        root_ty.clone()
    };
    let mut chain_offset: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        match &current_ty {
            RType::Struct { path, type_args, .. } => {
                let struct_path = path.clone();
                let struct_args = type_args.clone();
                let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
                let env = make_struct_env(&entry.type_params, &struct_args);
                let mut field_offset: u32 = 0;
                let mut found_field = false;
                let mut j = 0;
                while j < entry.fields.len() {
                    let fty = substitute_rtype(&entry.fields[j].ty, &env);
                    let s = byte_size_of(&fty, ctx.structs, ctx.enums);
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
            }
            RType::Tuple(elems) => {
                let elems = elems.clone();
                let idx: usize = chain[i]
                    .parse()
                    .expect("typeck verified tuple-index segment");
                let mut elem_offset: u32 = 0;
                let mut j = 0;
                while j < idx {
                    elem_offset += byte_size_of(&elems[j], ctx.structs, ctx.enums);
                    j += 1;
                }
                chain_offset += elem_offset;
                current_ty = elems[idx].clone();
            }
            _ => unreachable!("typeck verified chain navigates structs/tuples"),
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
            Storage::MemoryAt { addr_local } => {
                load_flat_from_memory(
                    ctx,
                    &current_ty,
                    BaseAddr::WasmLocal(*addr_local),
                    chain_offset,
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
    let mut current_ty = ctx.locals[binding_idx].rtype.clone();
    let mut i = 1;
    while i < chain.len() {
        current_ty = match chain[i].parse::<u32>() {
            Ok(idx) => extract_tuple_elem_from_stack(ctx, &current_ty, idx)?,
            Err(_) => extract_field_from_stack(ctx, &current_ty, &chain[i])?,
        };
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
        RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
        RType::Ref { inner, .. } => match inner.as_ref() {
            RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
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
    // `&arr[idx]` / `&mut arr[idx]` — bypass the place-chain
    // machinery and synthesize the equivalent of
    // `arr.index(idx)` / `arr.index_mut(idx)`. The method's return
    // type is already `&Output` / `&mut Output`, which matches what
    // a borrow is expected to produce.
    if let ExprKind::Index { base, index, .. } = &inner.kind {
        let (callee_idx, _ret_rt) = resolve_index_callee(ctx, base, mutable);
        emit_index_recv(ctx, base, mutable)?;
        codegen_expr(ctx, index)?;
        ctx.instructions.push(wasm::Instruction::Call(callee_idx));
        // The wasm result is one i32 — the &Output (or &mut Output).
        // Recover the pointee type from the typeck's recorded type
        // for the index expression.
        let elem_ty = ctx
            .expr_types[inner.id as usize]
            .as_ref()
            .expect("typeck recorded index expr type")
            .clone();
        let elem_ty = substitute_rtype(&elem_ty, &ctx.env);
        return Ok(RType::Ref {
            inner: Box::new(elem_ty),
            mutable,
            lifetime: crate::typeck::LifetimeRepr::Inferred(0),
        });
    }
    // `&*ptr` / `&mut *ptr` is a place borrow (a *re*-borrow, when
    // `ptr` is a ref; a raw-to-safe transition, when `ptr` is a raw
    // pointer). The result's address is `ptr`'s value — no value
    // copy, no fresh slot. Codegen the inner pointer expression
    // directly and use its i32 as the borrow's address.
    if let ExprKind::Deref(ptr_expr) = &inner.kind {
        let ptr_ty = codegen_expr(ctx, ptr_expr)?;
        let pointee = match ptr_ty {
            RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => *inner,
            _ => unreachable!("typeck verified deref target is a ref/raw-ptr"),
        };
        return Ok(RType::Ref {
            inner: Box::new(pointee),
            mutable,
            lifetime: crate::typeck::LifetimeRepr::Inferred(0),
        });
    }
    // Non-place inner (e.g. `&42`, `&foo()`): codegen the inner
    // expression's value, spill to a fresh shadow-stack slot, push
    // the slot's address. The slot lives until the function exits
    // (saved-SP epilogue reclaims). This is what a Rust temporary
    // borrow lowers to.
    let chain = match extract_place(inner) {
        Some(c) => c,
        None => {
            let inner_ty = codegen_expr(ctx, inner)?;
            let bytes = byte_size_of(&inner_ty, ctx.structs, ctx.enums);
            ctx.instructions
                .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
            ctx.instructions
                .push(wasm::Instruction::I32Const(bytes as i32));
            ctx.instructions.push(wasm::Instruction::I32Sub);
            ctx.instructions
                .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
            let addr_local = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions
                .push(wasm::Instruction::GlobalGet(SP_GLOBAL));
            ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
            // The inner's flat scalars are on the wasm stack; store
            // them at addr_local.
            store_flat_to_memory(ctx, &inner_ty, BaseAddr::WasmLocal(addr_local), 0);
            ctx.instructions
                .push(wasm::Instruction::LocalGet(addr_local));
            return Ok(RType::Ref {
                inner: Box::new(inner_ty),
                mutable,
                lifetime: crate::typeck::LifetimeRepr::Inferred(0),
            });
        }
    };
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
    let root_ty = ctx.locals[binding_idx].rtype.clone();
    // Borrowing `&r.field…` where r is a ref binding doesn't take r's address —
    // it takes the *pointee's* field address. The base is r's i32 value, not
    // SP+frame_offset. (For chain.len() == 1, falls into the SP-relative path
    // below — `&r` *does* take r's address, producing `&&T`.)
    let through_ref = matches!(&root_ty, RType::Ref { .. }) && chain.len() >= 2;

    // Walk chain to byte offset + final type.
    let mut current_ty = if through_ref {
        match &root_ty {
            RType::Ref { inner, .. } => (**inner).clone(),
            _ => unreachable!(),
        }
    } else {
        root_ty.clone()
    };
    let mut chain_offset: u32 = 0;
    let mut i = 1;
    while i < chain.len() {
        let (struct_path, struct_args) = match &current_ty {
            RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
            _ => unreachable!("typeck verified chain navigates structs"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let env = make_struct_env(&entry.type_params, &struct_args);
        let mut field_offset: u32 = 0;
        let mut j = 0;
        let mut found_field = false;
        while j < entry.fields.len() {
            let fty = substitute_rtype(&entry.fields[j].ty, &env);
            let s = byte_size_of(&fty, ctx.structs, ctx.enums);
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
        // Let-binding spilled to a fixed frame offset, or a pattern
        // binding allocated to a shadow-stack slot via MemoryAt.
        // (Local-storage bindings shouldn't reach here: escape
        // analysis would have flagged them addressed and forced
        // spilling at bind time.)
        match &ctx.locals[binding_idx].storage {
            Storage::Memory { frame_offset } => {
                let total = *frame_offset + chain_offset;
                let fb = ctx.frame_base_local;
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(fb));
                if total != 0 {
                    ctx.instructions
                        .push(wasm::Instruction::I32Const(total as i32));
                    ctx.instructions.push(wasm::Instruction::I32Add);
                }
            }
            Storage::MemoryAt { addr_local } => {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(*addr_local));
                if chain_offset != 0 {
                    ctx.instructions
                        .push(wasm::Instruction::I32Const(chain_offset as i32));
                    ctx.instructions.push(wasm::Instruction::I32Add);
                }
            }
            Storage::Local { .. } => {
                unreachable!("escape analysis must have spilled this binding");
            }
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
            let fb = ctx.frame_base_local;
            let temp = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions
                .push(wasm::Instruction::LocalGet(fb));
            ctx.instructions
                .push(wasm::Instruction::I32Load { align: 0, offset: off });
            ctx.instructions.push(wasm::Instruction::LocalSet(temp));
            temp
        }
        Storage::MemoryAt { addr_local } => {
            let src = *addr_local;
            let temp = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions.push(wasm::Instruction::LocalGet(src));
            ctx.instructions
                .push(wasm::Instruction::I32Load { align: 0, offset: 0 });
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
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => (**inner).clone(),
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
        // Spilled-binding accesses use the stable frame base captured
        // post-prologue, not the live `__sp` (which can drift during
        // the body from literal-borrow temps, enum construction, sret
        // allocations, etc.).
        BaseAddr::StackPointer => ctx
            .instructions
            .push(wasm::Instruction::LocalGet(ctx.frame_base_local)),
        BaseAddr::WasmLocal(i) => ctx.instructions.push(wasm::Instruction::LocalGet(i)),
    }
}

// Copy `bytes` bytes from address in `src_local` to address in
// `dst_local`. Emits a sequence of i64.load + i64.store for the bulk,
// then i32.load/store for any 4-byte tail. Used by sret returns to
// move the constructed enum from a callee-frame temp to the caller-
// supplied destination before SP restore.
fn emit_memcpy(ctx: &mut FnCtx, dst_local: u32, src_local: u32, bytes: u32) {
    let mut off: u32 = 0;
    while off + 8 <= bytes {
        ctx.instructions.push(wasm::Instruction::LocalGet(dst_local));
        ctx.instructions.push(wasm::Instruction::LocalGet(src_local));
        ctx.instructions.push(wasm::Instruction::I64Load { align: 3, offset: off });
        ctx.instructions.push(wasm::Instruction::I64Store { align: 3, offset: off });
        off += 8;
    }
    while off + 4 <= bytes {
        ctx.instructions.push(wasm::Instruction::LocalGet(dst_local));
        ctx.instructions.push(wasm::Instruction::LocalGet(src_local));
        ctx.instructions.push(wasm::Instruction::I32Load { align: 2, offset: off });
        ctx.instructions.push(wasm::Instruction::I32Store { align: 2, offset: off });
        off += 4;
    }
    while off + 2 <= bytes {
        ctx.instructions.push(wasm::Instruction::LocalGet(dst_local));
        ctx.instructions.push(wasm::Instruction::LocalGet(src_local));
        ctx.instructions.push(wasm::Instruction::I32Load16U { align: 1, offset: off });
        ctx.instructions.push(wasm::Instruction::I32Store16 { align: 1, offset: off });
        off += 2;
    }
    while off < bytes {
        ctx.instructions.push(wasm::Instruction::LocalGet(dst_local));
        ctx.instructions.push(wasm::Instruction::LocalGet(src_local));
        ctx.instructions.push(wasm::Instruction::I32Load8U { align: 0, offset: off });
        ctx.instructions.push(wasm::Instruction::I32Store8 { align: 0, offset: off });
        off += 1;
    }
}

// Pop flat scalars off the WASM stack and store them at base+offset+leaf_offset
// in memory. For enum-typed values, the wasm-stack value is an i32
// address, but the destination expects the enum's bytes inlined — so
// we memcpy `byte_size_of(enum)` bytes from the source address to
// dest+base_offset rather than just storing the address as a single
// i32 leaf.
fn store_flat_to_memory(ctx: &mut FnCtx, ty: &RType, base: BaseAddr, base_offset: u32) {
    if matches!(ty, RType::Enum { .. }) {
        // Stash source address from the wasm stack.
        let src_local = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        ctx.instructions.push(wasm::Instruction::LocalSet(src_local));
        // Stash dest address (= base + base_offset) into a fresh local.
        let dst_local = ctx.next_wasm_local;
        ctx.extra_locals.push(wasm::ValType::I32);
        ctx.next_wasm_local += 1;
        emit_base(ctx, base);
        if base_offset != 0 {
            ctx.instructions
                .push(wasm::Instruction::I32Const(base_offset as i32));
            ctx.instructions.push(wasm::Instruction::I32Add);
        }
        ctx.instructions.push(wasm::Instruction::LocalSet(dst_local));
        let bytes = byte_size_of(ty, ctx.structs, ctx.enums);
        emit_memcpy(ctx, dst_local, src_local, bytes);
        return;
    }
    let mut leaves: Vec<MemLeaf> = Vec::new();
    collect_leaves(ty, ctx.structs, ctx.enums, 0, &mut leaves);
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
    if matches!(ty, RType::Enum { .. }) {
        // The enum lives inline at [base + base_offset] (because
        // `store_flat_to_memory` for enums memcpys the disc + payload
        // into the slot rather than storing just the i32 address).
        // The "value" of the enum binding is its address — i.e., the
        // slot's address — so push that, not a load from offset 0
        // (which would yield the disc).
        emit_base(ctx, base);
        if base_offset != 0 {
            ctx.instructions
                .push(wasm::Instruction::I32Const(base_offset as i32));
            ctx.instructions.push(wasm::Instruction::I32Add);
        }
        return;
    }
    let mut leaves: Vec<MemLeaf> = Vec::new();
    collect_leaves(ty, ctx.structs, ctx.enums, 0, &mut leaves);
    let mut k = 0;
    while k < leaves.len() {
        emit_base(ctx, base);
        ctx.instructions.push(load_instr(&leaves[k], base_offset));
        k += 1;
    }
}
