use crate::ast::{Function, Item, Module, Path, Pattern};
use crate::layout::BindingStorageKind;
use crate::span::Error;
use crate::typeck::{
    CallResolution, FuncTable, GenericTemplate, IntKind, MethodResolution, RType, ReceiverAdjust,
    StructTable, byte_size_of, flatten_rtype, func_lookup, int_kind_name, struct_lookup,
    substitute_rtype,
};
use crate::wasm;

// Globals seeded by `lib.rs`: index 0 is the shadow-stack pointer
// (`__sp`); index 1 is the heap top (`__heap_top`, bump-allocator
// cursor for `¤alloc`).
const SP_GLOBAL: u32 = 0;
const HEAP_GLOBAL: u32 = 1;

// Codegen-side state: owns the per-crate mono table (delegated to
// `mono::MonoTable`) plus the string-literal pool. The mono table is
// populated eagerly by `mono::expand` before any byte emission begins;
// `intern` calls inside body-walking serve as idempotent lookups
// against the pre-populated table.
struct MonoState {
    mono_table: crate::mono::MonoTable,
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

impl MonoState {
    fn new(start_idx: u32, str_pool_base_offset: u32) -> Self {
        Self {
            mono_table: crate::mono::MonoTable::new(start_idx),
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

    fn intern(&mut self, template_idx: usize, type_args: Vec<RType>) -> u32 {
        self.mono_table.intern(template_idx, type_args)
    }

    fn next_idx(&self) -> u32 {
        self.mono_table.next_idx()
    }
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
    next_idx: &mut u32,
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
    // Mono idx allocation pulls from the shared `next_idx` so that
    // monomorphizations in *this* crate's codegen don't collide with
    // entries that the *next* crate's typeck will register at the
    // same idx. (Concretely: stdlib codegen monomorphizing a generic
    // helper claims an idx; without bumping `next_idx`, the user
    // crate's typeck would later register `answer` at the same idx
    // and the wasm export would point at the mono'd helper's body.)
    let mut mono = MonoState::new(*next_idx, str_pool_base_offset);
    // Eager mono expansion: walk every reachable function body and
    // intern each (template, args) pair before any byte emission. Then
    // codegen iterates the populated mono table to emit bodies. Any
    // dispatch site that still calls `mono.intern` during emission
    // hits an idempotent lookup against the pre-populated table.
    crate::mono::expand(root, structs, enums, traits, funcs, &mut mono.mono_table)?;
    emit_module(wasm_mod, root, &mut module_path, structs, enums, traits, funcs, &mut mono)?;
    // Iterate the mono table by index (entries may grow if expansion
    // missed a site and codegen's intern allocates a new entry — the
    // index walk picks those up too). For each entry, emit the
    // monomorphic body.
    let mut i = 0;
    while i < mono.mono_table.len() {
        let (template_idx, args_ref, wasm_idx) = mono.mono_table.entry(i);
        let type_args = args_ref.clone();
        emit_monomorphic(
            wasm_mod,
            template_idx,
            type_args,
            wasm_idx,
            structs,
            enums,
            traits,
            funcs,
            &mut mono,
        )?;
        i += 1;
    }
    *next_idx = mono.next_idx();
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
    // Pre-computed scope-end drop decision (`is_drop` + move-status
    // lookup), centralized in `layout::compute_drop_action` and stashed
    // here at decl time so scope-end drop emission doesn't recompute.
    drop_action: crate::layout::DropAction,
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
    // Per-binding storage kind, keyed by `BindingId` (parallel to
    // `mono_body.locals`). Populated from `MonoLayout.binding_storage`
    // at FnCtx construction. Codegen consults this to pick `Storage`
    // for each let, pattern leaf, param, or synthesized binding.
    binding_storage: Vec<BindingStorageKind>,
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
    mono: &'a mut MonoState,
    // Stack of enclosing loops (innermost-last). Each frame records the
    // wasm structured-control-flow depth at the loop's entry, used to
    // compute the right `Br` index for break/continue. `loop_depth` is
    // the depth of the wasm `Loop` instruction (= continue target);
    // `break_depth` is the depth of the wrapping `Block` (= break
    // target).
    loops: Vec<LoopCgFrame>,
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
    // Phase 1c: when codegen takes the Mono-driven body path, this is
    // set to the lowered MonoBody so `codegen_mono_*` handlers can look
    // up `MonoLocal` info by `BindingId`. None when AST path is used.
    mono_body: Option<&'a crate::mono::MonoBody>,
    // Per-Mono-BindingId: index into `ctx.locals` where the binding
    // currently lives (Some(idx)) or hasn't yet been declared / has
    // been truncated by an inner block's scope-end (None / stale).
    // Set up before Mono codegen runs (params get identity mapping)
    // and updated by `codegen_mono_stmt`'s Let handler. Inner block
    // truncation may leave entries pointing at indices that no
    // longer exist, but no live BindingId reference points there
    // (`lower_to_mono` enforces lexical scoping).
    mono_binding_to_local: Vec<Option<u32>>,
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
            Item::TypeAlias(_) => {}
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
    template_idx: usize,
    type_args: Vec<RType>,
    wasm_idx: u32,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let tmpl = &funcs.templates[template_idx];
    let input = build_mono_input_for_template(tmpl, type_args, wasm_idx);
    let mono_fn = crate::mono::lower_to_mono(
        &input,
        structs,
        enums,
        traits,
        funcs,
        &mono.mono_table,
    )?;
    emit_function_concrete(
        wasm_mod,
        &mono_fn,
        structs,
        enums,
        traits,
        funcs,
        mono,
    )
}

// Build a `MonoFnInput` for one (template, concrete type_args) pair:
// substitute every Param-bearing artifact through the env so the input
// is fully concrete. The caller passes this to `lower_to_mono` which
// produces the codegen-ready `MonoFn`.
fn build_mono_input_for_template<'a>(
    tmpl: &'a GenericTemplate,
    type_args: Vec<RType>,
    wasm_idx: u32,
) -> crate::mono::MonoFnInput<'a> {
    let env = build_env(&tmpl.type_params, &type_args);
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
    crate::mono::MonoFnInput {
        func: &tmpl.func,
        param_types,
        return_type,
        expr_types,
        method_resolutions,
        call_resolutions,
        builtin_type_targets,
        moved_places,
        move_sites,
        // Pattern ergonomics carry through monomorphization unchanged
        // — auto-peel decisions depend only on the *shape* of the
        // scrutinee type (ref vs not), and `&T`-vs-`&U` substitutions
        // preserve that shape.
        pattern_ergo: tmpl.pattern_ergo.clone(),
        wasm_idx,
        is_export: false, // monomorphic instances are never exported
    }
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
    let _ = self_target; // impl-target propagation lives in build_mono_input_for_template; non-generic paths don't need it
    let input = crate::mono::MonoFnInput {
        func,
        param_types: entry.param_types.clone(),
        return_type: entry.return_type.clone(),
        expr_types: entry.expr_types.clone(),
        method_resolutions: entry.method_resolutions.clone(),
        call_resolutions: entry.call_resolutions.clone(),
        builtin_type_targets: clone_btt(&entry.builtin_type_targets),
        moved_places: clone_moved_places(&entry.moved_places),
        move_sites: clone_move_sites(&entry.move_sites),
        pattern_ergo: entry.pattern_ergo.clone(),
        wasm_idx: entry.idx,
        is_export: current_module.is_empty() && path_prefix.len() == current_module.len(),
    };
    let mono_fn = crate::mono::lower_to_mono(
        &input,
        structs,
        enums,
        traits,
        funcs,
        &mono.mono_table,
    )?;
    emit_function_concrete(
        wasm_mod,
        &mono_fn,
        structs,
        enums,
        traits,
        funcs,
        mono,
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
    mono_fn: &crate::mono::MonoFn,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &crate::typeck::TraitTable,
    funcs: &FuncTable,
    mono: &mut MonoState,
) -> Result<(), Error> {
    let body = &mono_fn.body;
    let param_types = &mono_fn.param_types;
    let return_type = &mono_fn.return_type;
    let wasm_idx = mono_fn.wasm_idx;
    let is_export = mono_fn.is_export;
    // Per-mono frame layout pass: walks the Mono IR to mark addressed
    // bindings + compute per-binding storage kind + frame offsets.
    // Codegen reads `binding_storage` (BindingId-keyed) directly.
    let layout = crate::layout::compute_mono_layout(
        body,
        &mono_fn.moved_places,
        structs,
        enums,
        traits,
    );
    let frame_size = layout.frame_size;
    // Param storage is the prefix of binding_storage corresponding to
    // BindingOrigin::Param entries. Lowering declares params first in
    // BindingId order, so binding_storage[0..param_count] matches.
    let param_count = mono_fn.param_types.len();
    let param_storage: Vec<BindingStorageKind> =
        layout.binding_storage[..param_count].to_vec();
    if std::env::var("MONO_TRACE").is_ok() {
        eprintln!("MONO_OK fn={}", mono_fn.name);
    }
    let _ = funcs; // currently unused by emit_function_concrete (lowering, which needed it, ran upstream)

    // Build the WASM signature: refs collapse to a single i32; everything else
    // flattens to flat scalars as before. Functions returning enums use sret:
    // a leading i32 param is prepended (the destination address into the
    // caller's frame), and at function-body end we memcpy the constructed
    // enum's bytes to that address before returning.
    let returns_enum = matches!(return_type, Some(RType::Enum { .. }));
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
    while k < param_count {
        let pty = param_types[k].clone();
        let storage = match param_storage[k] {
            BindingStorageKind::Memory { frame_offset } => Storage::Memory { frame_offset },
            BindingStorageKind::Local => {
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
            BindingStorageKind::MemoryAt => unreachable!("params are never MemoryAt"),
        };
        if matches!(param_storage[k], BindingStorageKind::Memory { .. }) {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&pty, structs, &mut vts);
            let mut j = 0;
            while j < vts.len() {
                wasm_params.push(vts[j].copy());
                next_wasm_local += 1;
                j += 1;
            }
        }
        // Param names live on body.locals[k] (lowering declares params
        // first in BindingId order — see `lower_to_mono`).
        let pname = body.locals[k].name.clone();
        let drop_action = crate::layout::compute_drop_action(
            &pname,
            &pty,
            &mono_fn.moved_places,
            structs,
            enums,
            traits,
        );
        locals.push(LocalBinding {
            name: pname,
            rtype: pty,
            storage,
            drop_action,
        });
        k += 1;
    }

    let mut wasm_results: Vec<wasm::ValType> = Vec::new();
    if let Some(rt) = return_type {
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
        binding_storage: layout.binding_storage.clone(),
        moved_places: clone_moved_places(&mono_fn.moved_places),
        move_sites: clone_move_sites(&mono_fn.move_sites),
        drop_flags: Vec::new(),
        pending_types: Vec::new(),
        pending_types_base: wasm_mod.types.len() as u32,
        mono,
        loops: Vec::new(),
        frame_base_local: 0,
        fn_entry_sp_local: 0,
        sret_ptr_local,
        return_flat: return_flat_for_ctx,
        return_rt: return_type.clone(),
        mono_body: None,
        mono_binding_to_local: Vec::new(),
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
        while p < param_count {
            let pty = ctx.locals[p].rtype.clone();
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&pty, structs, &mut vts);
            let flat_size = vts.len() as u32;
            if let BindingStorageKind::Memory { frame_offset: off } = param_storage[p] {
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
                    if off != 0 {
                        ctx.instructions
                            .push(wasm::Instruction::I32Const(off as i32));
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
                        ctx.instructions.push(store_instr(&leaves[k], off));
                        k += 1;
                    }
                }
            }
            wasm_local_cursor += flat_size;
            p += 1;
        }
    }

    // Allocate drop flags for any param that's MaybeMoved at scope-end.
    // Init = 1 (param always initialized at fn entry).
    let mut p = 0;
    while p < param_count {
        let local_name = ctx.locals[p].name.clone();
        if matches!(ctx.locals[p].drop_action, crate::layout::DropAction::Flagged) {
            let flag_idx = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.drop_flags.push((local_name, flag_idx));
            ctx.instructions.push(wasm::Instruction::I32Const(1));
            ctx.instructions.push(wasm::Instruction::LocalSet(flag_idx));
        }
        p += 1;
    }

    // Phase 1c step 2: try Mono-driven body codegen if available and
    // The MonoBody was lowered upstream (in `emit_function` for
    // non-generics, in `emit_monomorphic` for generic instances); the
    // codegen-side per-variant support check is the only remaining gate
    // before byte emission.
    if !mono_codegen_supports(&body) {
        return Err(Error {
            file: String::new(),
            message: format!(
                "codegen: Mono codegen rejected fn `{}` ({})",
                mono_fn.name,
                first_unsupported_block(&body.body),
            ),
            // No precise span available — MonoBody is post-lowering and
            // doesn't carry the source range of the entire function body.
            // This error is a compiler invariant violation, not a user
            // diagnostic; the message identifies the fn by name.
            span: crate::span::Span::new(
                crate::span::Pos::new(1, 1),
                crate::span::Pos::new(1, 1),
            ),
        });
    }
    ctx.mono_body = Some(body);
    // Initialize BindingId → ctx.locals map. Params have identity
    // mapping (they were pushed in BindingId order before Mono codegen
    // runs). Other entries (lets) get populated by `codegen_mono_stmt`'s
    // Let handler.
    ctx.mono_binding_to_local = vec![None; body.locals.len()];
    let mut k = 0;
    while k < body.locals.len() {
        if let crate::mono::BindingOrigin::Param(idx) = &body.locals[k].origin {
            ctx.mono_binding_to_local[k] = Some(*idx as u32);
        }
        k += 1;
    }
    codegen_mono_block(&mut ctx, &body.body)?;
    ctx.mono_body = None;
    ctx.mono_binding_to_local = Vec::new();

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
            name: mono_fn.name.clone(),
            kind: wasm::ExportKind::Func,
            index: func_idx,
        });
    }

    Ok(())
}

// ============================================================================
// Statement / expression codegen
// ============================================================================




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

// `for pat in iter { body }` — emit equivalent of:
//
//   let mut __iter = iter;
//   loop {
//       let __opt: Option<Item> = Iterator::next(&mut __iter);
//       match __opt {
//           Some(pat) => body,
//           None => break,
//       }
//   }
//
// The iter is moved into a shadow-stack slot so we can pass `&mut
// __iter` (an i32 address) to `Iterator::next`. Each iteration
// allocates a fresh `Option<Item>` sret slot below SP, calls
// `next`, reads the discriminant; disc=0 (None — Option declares
// None first) exits the loop, disc=1 (Some) loads the Item payload
// from `addr+4` into the binding's wasm locals and runs the body.
//
// Scope limits (intentional MVP): the pattern must be a single
// `Var(name)` (or `_`); destructuring patterns inside `for` are
// rejected by borrowck's `materialize_for_loop_bindings`. The iter's
// type and `Item` type can have any shape — `flatten_rtype` /
// `store_flat_to_memory` / `load_flat_from_memory` handle multi-
// scalar layouts uniformly.



// `return EXPR` / `return`. Mirrors the function-end epilogue:
// 1. Codegen the value (or unit). Stash to fresh wasm locals so we
//    can run drops without disturbing the value.
// 2. Drop every in-scope binding (whole `ctx.locals`).
// 3. For sret-returning functions: memcpy the value's bytes to the
//    caller-supplied sret slot, then push the sret slot's address.
// 4. For non-sret functions: push the stashed flat scalars back.
// 5. Restore SP from `fn_entry_sp_local`.
// 6. Emit wasm `Return`.

// `arr[idx]` in value position — synthesize the equivalent of
// `*<Index>::index(&arr, idx)`. Resolves the impl via `solve_impl`,
// emits the recv as a borrow of base (matching `&self`), the idx,
// and the call; the call returns `&Output` as one i32 (the
// pointer), and we then load the `Output`'s flat scalars from that
// address. Caller-context-aware variants for assign/borrow contexts
// live in `codegen_index_ref` and `codegen_index_assign`.

// Push the right receiver value for an Index/IndexMut method call.
// If `base`'s type already matches the method's `&self` shape — i.e.
// `base` is `&T` (for Index) or `&mut T` (for IndexMut) — pass it
// through with `codegen_expr`. Otherwise (base is owned `T` or
// matches in some other ref-permutation) take `&base` / `&mut base`
// via `codegen_borrow`.

// Resolve the wasm idx + return type of the appropriate Index /
// IndexMut method for `base`'s type. `mutable=true` selects
// IndexMut::index_mut. Used by both value-position indexing and the
// (future) borrow / assign paths.

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
        let action = ctx.locals[i].drop_action;
        if matches!(action, crate::layout::DropAction::Skip) {
            continue;
        }
        // Drop requires `&mut binding` — only addressed bindings can be
        // dropped this way. Both Storage::Memory (frame slot at known
        // offset) and Storage::MemoryAt (shadow-stack slot whose addr
        // lives in a wasm local) are addressable and supported below.
        // Drop params aren't yet auto-addressed so they're silently
        // skipped here (a known limitation).
        if !matches!(
            &ctx.locals[i].storage,
            Storage::Memory { .. } | Storage::MemoryAt { .. }
        ) {
            continue;
        }
        let rt = ctx.locals[i].rtype.clone();
        match action {
            crate::layout::DropAction::Skip => continue,
            crate::layout::DropAction::Flagged => {
                // Flagged drop: `if flag { drop }; end`. The flag was
                // initialized to 1 at decl, cleared to 0 at every move
                // site walked through this path, so it correctly
                // reflects whether the storage still owns its value.
                let flag_idx = lookup_drop_flag(&ctx.drop_flags, &ctx.locals[i].name)
                    .expect("Flagged binding must have an allocated drop flag");
                ctx.instructions.push(wasm::Instruction::LocalGet(flag_idx));
                ctx.instructions.push(wasm::Instruction::If(wasm::BlockType::Empty));
                emit_drop_call_for_local(ctx, i, &rt)?;
                ctx.instructions.push(wasm::Instruction::End);
            }
            crate::layout::DropAction::Always => {
                emit_drop_call_for_local(ctx, i, &rt)?;
            }
        }
    }
    Ok(())
}

fn emit_drop_call_for_local(
    ctx: &mut FnCtx,
    idx: usize,
    rtype: &RType,
) -> Result<(), Error> {
    // Materialize the binding's address into a fresh wasm i32 local,
    // then hand off to the recursive walker. Two storage shapes carry
    // an address:
    //   Storage::Memory   — fixed frame_offset relative to
    //                       frame_base_local. Use frame_base_local
    //                       (not live __sp): by the time scope-end
    //                       drops fire, body may have drifted __sp
    //                       via literal-borrow temps, sret slots, or
    //                       enum construction.
    //   Storage::MemoryAt — addr stashed in a wasm local at the
    //                       point of binding (used by codegen_pattern
    //                       for addressed pattern bindings).
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    match &ctx.locals[idx].storage {
        Storage::Memory { frame_offset } => {
            let off = *frame_offset;
            ctx.instructions
                .push(wasm::Instruction::LocalGet(ctx.frame_base_local));
            if off != 0 {
                ctx.instructions.push(wasm::Instruction::I32Const(off as i32));
                ctx.instructions.push(wasm::Instruction::I32Add);
            }
        }
        Storage::MemoryAt { addr_local: src } => {
            ctx.instructions.push(wasm::Instruction::LocalGet(*src));
        }
        _ => unreachable!("Drop binding must be address-marked"),
    }
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    emit_drop_walker(ctx, rtype, addr_local)
}

// Recursive scope-end drop emission. Lays out the destruction of a
// single value in `addr_local` to match Rust's drop-glue semantics:
// (1) call the user's `Drop::drop` if `ty` directly impls Drop;
// (2) then drop each contained sub-place that needs_drop, in source
//     order — struct/tuple fields in declaration order, enum payload
//     fields after a discriminant dispatch.
//
// Mirrors how rustc synthesizes `core::ptr::drop_in_place::<T>`.
// A type with both a direct Drop impl AND Drop-typed fields runs its
// own drop body first; the field walker fires afterwards (the user
// code observes pre-drop field values, then the implicit walker
// finalizes).
fn emit_drop_walker(
    ctx: &mut FnCtx,
    ty: &RType,
    addr_local: u32,
) -> Result<(), Error> {
    if crate::typeck::is_drop(ty, ctx.traits) {
        let callee_idx = resolve_drop_method_idx(ctx, ty);
        ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
        ctx.instructions.push(wasm::Instruction::Call(callee_idx));
    }
    match ty {
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(ctx.structs, path).expect("resolved struct");
            let env = make_struct_env(&entry.type_params, type_args);
            let mut byte_off: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                if crate::typeck::needs_drop(&fty, ctx.structs, ctx.enums, ctx.traits) {
                    let field_addr = emit_address_at_offset(ctx, addr_local, byte_off);
                    emit_drop_walker(ctx, &fty, field_addr)?;
                }
                byte_off += byte_size_of(&fty, ctx.structs, ctx.enums);
                i += 1;
            }
        }
        RType::Tuple(elems) => {
            let mut byte_off: u32 = 0;
            let mut i = 0;
            while i < elems.len() {
                if crate::typeck::needs_drop(&elems[i], ctx.structs, ctx.enums, ctx.traits) {
                    let elem_addr = emit_address_at_offset(ctx, addr_local, byte_off);
                    emit_drop_walker(ctx, &elems[i], elem_addr)?;
                }
                byte_off += byte_size_of(&elems[i], ctx.structs, ctx.enums);
                i += 1;
            }
        }
        RType::Enum { path, type_args, .. } => {
            emit_enum_variant_walker(ctx, path, type_args, addr_local)?;
        }
        _ => {}
    }
    Ok(())
}

// Resolve `<ty as Drop>::drop` to a wasm function index, monomorphizing
// the impl-method template against the impl's type-arg substitution
// when needed. Factored out of the original emit_drop_call_for_local.
fn resolve_drop_method_idx(ctx: &mut FnCtx, ty: &RType) -> u32 {
    let drop_path = crate::typeck::drop_trait_path();
    let resolution = crate::typeck::solve_impl(&drop_path, ty, ctx.traits, 0)
        .expect("is_drop verified Drop impl exists");
    let cand = crate::typeck::find_trait_impl_method(
        ctx.funcs,
        resolution.impl_idx,
        "drop",
    )
    .expect("Drop impl provides drop method");
    match cand {
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
    }
}

// Allocate a fresh wasm i32 local holding `addr_local + byte_off`.
// Used by the drop walker to point at sub-places (struct fields,
// tuple elements, enum payload fields) without recomputing the base
// across recursive walker calls.
fn emit_address_at_offset(ctx: &mut FnCtx, addr_local: u32, byte_off: u32) -> u32 {
    let dest = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
    if byte_off != 0 {
        ctx.instructions.push(wasm::Instruction::I32Const(byte_off as i32));
        ctx.instructions.push(wasm::Instruction::I32Add);
    }
    ctx.instructions.push(wasm::Instruction::LocalSet(dest));
    dest
}

// Drop the active variant's payload of an enum at `addr_local`.
// Layout reminder: i32 disc at offset 0, payload bytes start at offset
// 4. Variants whose payload contains no needs_drop field collapse out
// of the dispatch. If no variant needs walking, we emit nothing —
// the disc load itself is skipped.
fn emit_enum_variant_walker(
    ctx: &mut FnCtx,
    enum_path: &Vec<String>,
    type_args: &Vec<RType>,
    addr_local: u32,
) -> Result<(), Error> {
    let entry = crate::typeck::enum_lookup(ctx.enums, enum_path)
        .expect("resolved enum");
    let env = make_struct_env(&entry.type_params, type_args);
    // Collect (disc, payload-with-offsets) for variants that have at
    // least one needs_drop field; anything else can be safely skipped
    // when its disc is observed.
    struct PayloadField {
        ty: RType,
        byte_off: u32,
    }
    let mut variants_to_walk: Vec<(u32, Vec<PayloadField>)> = Vec::new();
    let mut vi = 0;
    while vi < entry.variants.len() {
        let v = &entry.variants[vi];
        let mut fields: Vec<PayloadField> = Vec::new();
        let mut off: u32 = 4;
        match &v.payload {
            crate::typeck::VariantPayloadResolved::Unit => {}
            crate::typeck::VariantPayloadResolved::Tuple(types) => {
                let mut i = 0;
                while i < types.len() {
                    let fty = substitute_rtype(&types[i], &env);
                    fields.push(PayloadField { ty: fty.clone(), byte_off: off });
                    off += byte_size_of(&fty, ctx.structs, ctx.enums);
                    i += 1;
                }
            }
            crate::typeck::VariantPayloadResolved::Struct(decls) => {
                let mut i = 0;
                while i < decls.len() {
                    let fty = substitute_rtype(&decls[i].ty, &env);
                    fields.push(PayloadField { ty: fty.clone(), byte_off: off });
                    off += byte_size_of(&fty, ctx.structs, ctx.enums);
                    i += 1;
                }
            }
        }
        let any_drop = fields.iter().any(|f| {
            crate::typeck::needs_drop(&f.ty, ctx.structs, ctx.enums, ctx.traits)
        });
        if any_drop {
            variants_to_walk.push((v.disc, fields));
        }
        vi += 1;
    }
    if variants_to_walk.is_empty() {
        return Ok(());
    }
    // Cache the discriminant in a wasm local so we can compare it
    // against each variant's disc without re-loading from memory.
    let disc_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
    ctx.instructions.push(wasm::Instruction::I32Load { align: 2, offset: 0 });
    ctx.instructions.push(wasm::Instruction::LocalSet(disc_local));
    // Emit a chain of `if disc == N { drop payload fields }` blocks.
    // Variants are independent (only one matches at runtime) so each
    // is its own If with no Else — falls through to the next when not
    // matching.
    let mut wi = 0;
    while wi < variants_to_walk.len() {
        let (disc, fields) = &variants_to_walk[wi];
        ctx.instructions.push(wasm::Instruction::LocalGet(disc_local));
        ctx.instructions.push(wasm::Instruction::I32Const(*disc as i32));
        ctx.instructions.push(wasm::Instruction::I32Eq);
        ctx.instructions
            .push(wasm::Instruction::If(wasm::BlockType::Empty));
        let mut fi = 0;
        while fi < fields.len() {
            let f = &fields[fi];
            if crate::typeck::needs_drop(&f.ty, ctx.structs, ctx.enums, ctx.traits) {
                let field_addr = emit_address_at_offset(ctx, addr_local, f.byte_off);
                emit_drop_walker(ctx, &f.ty, field_addr)?;
            }
            fi += 1;
        }
        ctx.instructions.push(wasm::Instruction::End);
        wi += 1;
    }
    Ok(())
}


// `let PAT = EXPR else { … };` lowering. Mirrors `codegen_if_let_expr`
// but with the polarity flipped (match falls through, no-match
// runs the diverging else block):
//
//   <codegen value into a stashed scrutinee>
//   block (Empty)            ; outer — control resumes here on match
//     block (Empty)          ; inner — pattern-test br_if-out lands here
//       <codegen_pattern>    ; on no-match, br 0 (out of inner)
//       <bindings live>      ; pushed onto ctx.locals by codegen_pattern
//       br 1                 ; matched — skip past the else block
//     end                    ; no-match path lands here
//     <codegen else block>   ; diverges (typeck-enforced)
//   end                      ; resumes here only on match
//
// After the outer Block ends, ctx.locals carries the new bindings,
// so the rest of the enclosing block sees them.

// The pre-pattern simple-binding lowering, factored out so the
// pattern-driven `codegen_let_stmt` can call it without reading
// `let_stmt.name` / `let_stmt.mutable` directly.


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



// `panic!(msg)` — codegen the &str arg (pushes ptr, len), call the
// imported `env.panic` (wasm function index 0), then `unreachable`.
// The expression's "result" is `!` so wasm validator accepts the
// dead code that follows.

// `(a, b, c)` — codegen each elem in source order, leaving its
// flat scalars on the wasm stack. The tuple's flat representation
// is the concatenation; `()` produces no instructions at all.

// `t.<index>` — analogous to `codegen_field_access`. Try the place-rooted
// path (chain bottoms at a Var or a chain through &/structs/tuples) for
// direct memory access; otherwise fall back to evaluating the whole base
// onto the stack and stash-extracting the element.

// Stack-position twin of `extract_field_from_stack`. The tuple's flat
// scalars are on the stack in declaration order; we need to keep only
// the slice belonging to element `index`.

// Lower `¤name(args)` to its wasm op(s). Args are codegen'd in order
// (left-to-right); then the corresponding wasm instruction is
// emitted. The result type is the same as what typeck returned for
// this Builtin's NodeId — read it back from `ctx.expr_types` rather
// than re-deriving from the name (saves a parse).

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

// ¤free(p: *mut u8). No-op stub today — evaluates `p` for its side
// effects (move tracking, etc.) and discards the address. The heap is
// pure bump-allocation; freed memory is not reclaimed. Provided as the
// future hook point for a real allocator.

// ¤cast::<A, B>(p: *X B) -> *X A (where X is const or mut, preserved).
// Pure no-op at runtime — raw pointers flatten to a single i32 address
// regardless of pointee type, so the wasm value passes through
// unchanged. Typeck has already validated the turbofish args.

// `¤slice_ptr::<T>(s: &[T]) -> *const T` and the mut variant
// `¤slice_mut_ptr::<T>(s: &mut [T]) -> *mut T`. The arg pushes
// (data_ptr, len); we want `data_ptr` (below `len`) and discard
// `len` (top). One `drop` does it.

// `¤slice_len::<T>(s: &[T]) -> usize`. The arg pushes (data_ptr, len)
// onto the stack; we want `len` (top) and discard `data_ptr` (below).
// Stash `len` to a temp local, drop the ptr, reload the temp.

// 1-arg pass-through used by `¤str_as_bytes`: codegen the single arg
// (which already flattens to the desired result shape) and use the
// builtin's recorded result type.

// `¤make_slice::<T>(ptr, len) -> &[T]`. Pure no-op at codegen: both
// args already flatten to one i32, leaving (ptr, len) on the wasm
// stack — exactly the fat-ref representation of `&[T]`.

// `¤size_of::<T>() -> usize`. Compile-time-constant: at this point T is
// concrete (after monomorphization), so we just emit `i32.const
// byte_size_of(T)`. The result type is `usize`, which flattens to an
// i32 on wasm32.

// `¤ptr_usize_add(p, n) -> *X T`, `¤ptr_usize_sub(p, n) -> *X T`,
// `¤ptr_isize_offset(p, n) -> *X T` — byte-wise pointer arithmetic.
// Raw pointers and usize/isize all flatten to wasm `i32` on wasm32, so
// each lowers to a single `i32.add` / `i32.sub`. Signed offsets use
// the same unsigned add (two's-complement: `p + (-1i32 as i32)` adds
// 0xFFFFFFFF, equivalent to `p - 1` in 32-bit arithmetic).

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
    // Mono lowering pre-substitutes the scrutinee type — `scrut_ty` is
    // always concrete by the time codegen_pattern runs.
    let resolved_scrut = scrut_ty.clone();
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
    let drop_action = crate::layout::compute_drop_action(
        name,
        &ref_ty,
        &ctx.moved_places,
        ctx.structs,
        ctx.enums,
        ctx.traits,
    );
    ctx.locals.push(LocalBinding {
        name: name.to_string(),
        rtype: ref_ty,
        storage: Storage::Local {
            wasm_start: dest,
            flat_size: 1,
        },
        drop_action,
    });
}

// Resolve a pattern leaf's `BindingId` by looking up its AST node id
// in the lowered body's locals (BindingOrigin::Pattern(nid)). Returns
// `None` if the pattern wasn't declared via `declare_pattern_bindings`
// (e.g. wildcard, refutable lit, etc.) — the caller treats that as
// "not addressed".
fn pattern_binding_id_for(
    ctx: &FnCtx,
    pattern_id: crate::ast::NodeId,
) -> Option<u32> {
    let body = ctx.mono_body?;
    let mut k = 0;
    while k < body.locals.len() {
        if let crate::mono::BindingOrigin::Pattern(nid) = &body.locals[k].origin {
            if *nid == pattern_id {
                return Some(k as u32);
            }
        }
        k += 1;
    }
    None
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
        let drop_action = crate::layout::compute_drop_action(
            name,
            ty,
            &ctx.moved_places,
            ctx.structs,
            ctx.enums,
            ctx.traits,
        );
        ctx.locals.push(LocalBinding {
            name: name.to_string(),
            rtype: ty.clone(),
            storage: Storage::Local {
                wasm_start: dest,
                flat_size: 1,
            },
            drop_action,
        });
        return;
    }
    // Non-enum value: if escape analysis flagged this binding as
    // addressed, allocate a shadow-stack slot up front so reads /
    // writes / borrows all share one stable location. Otherwise stash
    // into wasm locals (the fast path). Look up the pattern leaf's
    // BindingId via its origin (Pattern(pat.id)) in the lowered body.
    let addressed = pattern_binding_id_for(ctx, pattern_id)
        .map(|bid| matches!(ctx.binding_storage[bid as usize], BindingStorageKind::MemoryAt))
        .unwrap_or(false);
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
        let drop_action = crate::layout::compute_drop_action(
            name,
            ty,
            &ctx.moved_places,
            ctx.structs,
            ctx.enums,
            ctx.traits,
        );
        ctx.locals.push(LocalBinding {
            name: name.to_string(),
            rtype: ty.clone(),
            storage: Storage::MemoryAt { addr_local },
            drop_action,
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
    let drop_action = crate::layout::compute_drop_action(
        name,
        ty,
        &ctx.moved_places,
        ctx.structs,
        ctx.enums,
        ctx.traits,
    );
    ctx.locals.push(LocalBinding {
        name: name.to_string(),
        rtype: ty.clone(),
        storage: Storage::Local {
            wasm_start: dest_start,
            flat_size: vts.len() as u32,
        },
        drop_action,
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


// Trait-dispatched method call: substitute the recorded recv type
// against the mono env, run `solve_impl` to find the impl row, look up
// the method by name, then emit a regular call to its (possibly
// monomorphized) wasm idx.

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
        // Same class. For Narrow32→Narrow32 we still have to fix up the
        // representation when the target's bit width is strictly less
        // than the source's: u32→u8 must mask to 0xFF, i32→i8 must
        // sign-extend from 8 bits, etc. Same-class widening (u8→u32,
        // i8→i32, u16→u32) is a no-op because the source value is
        // already stored in its narrow-type representation (zero- or
        // sign-extended to 32 bits — see emit_narrow_width_fixup), so
        // the bit pattern matches the wider target. Wide64/Wide128
        // same-class transitions are also no-ops (only one wasm-level
        // shape per class above 32 bits).
        (IntClass::Narrow32, IntClass::Narrow32) => {
            if narrow_bit_width(tgt) < narrow_bit_width(src) {
                emit_narrow_width_fixup(ctx, int_kind_name(tgt));
            }
        }
        _ => {}
    }
}

fn narrow_bit_width(k: &IntKind) -> u32 {
    match k {
        IntKind::U8 | IntKind::I8 => 8,
        IntKind::U16 | IntKind::I16 => 16,
        _ => 32,
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



// Walk the spine of nested FieldAccess / Var nodes; if it bottoms out at a
// Var, push the root name and return true. Otherwise return false (and out is
// in an unspecified state — caller should drop it).

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
            Storage::Local { wasm_start, .. } => {
                // Non-spilled value: extract the field's flat scalars
                // directly from the binding's wasm-locals range. The
                // chain's leaf sits at `flat_chain_offset` scalars past
                // the start; push exactly its flat width.
                let flat_off = flat_chain_offset(ctx, chain, binding_idx);
                let mut leaf_vts: Vec<wasm::ValType> = Vec::new();
                flatten_rtype(&current_ty, ctx.structs, &mut leaf_vts);
                let mut k = 0;
                while k < leaf_vts.len() {
                    ctx.instructions
                        .push(wasm::Instruction::LocalGet(*wasm_start + flat_off + k as u32));
                    k += 1;
                }
            }
        }
    }
    Ok(current_ty)
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


// Smart-pointer deref via `Deref::deref(&inner)` (or
// `DerefMut::deref_mut(&mut inner)` when `mutable` is true). The
// trait method returns `&Target` / `&mut Target` — an i32 address —
// from which we then load the Target leaves.

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

// ============================================================================
// Phase 1c step 2: Mono-driven body codegen (initial trivial-only support).
//
// Recognized variants: MonoLit (all), Local (lookup against ctx.locals
// by binding name — same set the AST path uses), Block, Unsafe,
// Builtin (delegated via a small bridge that lowers args then calls
// the existing arithmetic emitter), Tuple of length 0 (the unit
// value). Everything else returns Err so codegen falls back to AST.
// Broader variant coverage in follow-up turns.
// ============================================================================

fn mono_codegen_supports(body: &crate::mono::MonoBody) -> bool {
    let ok = mono_supports_block(&body.body);
    if !ok && std::env::var("MONO_TRACE_REJECT").is_ok() {
        let why = first_unsupported_block(&body.body);
        eprintln!("MONO_REJECT body why={}", why);
    }
    ok
}

fn first_unsupported_block(b: &crate::mono::MonoBlock) -> String {
    let mut i = 0;
    while i < b.stmts.len() {
        if !mono_supports_stmt(&b.stmts[i]) {
            return first_unsupported_stmt(&b.stmts[i]);
        }
        i += 1;
    }
    if let Some(t) = &b.tail {
        if !mono_supports_expr(t) {
            return first_unsupported_expr(t);
        }
    }
    "<all supported?>".to_string()
}

fn first_unsupported_stmt(s: &crate::mono::MonoStmt) -> String {
    use crate::mono::MonoStmt as S;
    match s {
        S::Expr(e) => first_unsupported_expr(e),
        S::Let { value, .. } => first_unsupported_expr(value),
        S::Assign { place, value, .. } => {
            if !mono_supports_assign_place(place) {
                return format!("Assign-place: {}", place_kind_name(place));
            }
            first_unsupported_expr(value)
        }
        S::LetPattern { pattern, value, .. } => {
            if !irrefutable_pattern(pattern) {
                return "LetPattern refutable".to_string();
            }
            first_unsupported_expr(value)
        }
        _ => "MonoStmt other".to_string(),
    }
}

fn place_kind_name(p: &crate::mono::MonoPlace) -> String {
    use crate::mono::MonoPlaceKind as PK;
    match &p.kind {
        PK::Local(_) => "Local".to_string(),
        PK::Field { base, .. } => format!("Field({})", place_kind_name(base)),
        PK::TupleIndex { base, .. } => format!("TupleIndex({})", place_kind_name(base)),
        PK::Deref { inner } => match &inner.kind {
            crate::mono::MonoExprKind::Local(_, _) => "Deref(Local)".to_string(),
            other => format!("Deref({})", expr_kind_name(other)),
        },
    }
}

fn expr_kind_name(e: &crate::mono::MonoExprKind) -> &'static str {
    use crate::mono::MonoExprKind as K;
    match e {
        K::Lit(_) => "Lit",
        K::Local(_, _) => "Local",
        K::PlaceLoad(_) => "PlaceLoad",
        K::Borrow { .. } => "Borrow",
        K::BorrowOfValue { .. } => "BorrowOfValue",
        K::Call { .. } => "Call",
        K::MethodCall { .. } => "MethodCall",
        K::Builtin { .. } => "Builtin",
        K::StructLit { .. } => "StructLit",
        K::VariantConstruct { .. } => "VariantConstruct",
        K::Tuple(_) => "Tuple",
        K::Cast { .. } => "Cast",
        K::Match { .. } => "Match",
        K::Loop { .. } => "Loop",
        K::Block(_) => "Block",
        K::Unsafe(_) => "Unsafe",
        K::Break { .. } => "Break",
        K::Continue { .. } => "Continue",
        K::Return { .. } => "Return",
        K::MacroCall { .. } => "MacroCall",
    }
}

fn first_unsupported_expr(e: &crate::mono::MonoExpr) -> String {
    use crate::mono::MonoExprKind as K;
    if mono_supports_expr(e) {
        return "<supported>".to_string();
    }
    match &e.kind {
        K::Lit(_) => "Lit".to_string(),
        K::Local(_, _) => "Local".to_string(),
        K::PlaceLoad(p) => format!("PlaceLoad({})", place_kind_name(p)),
        K::Borrow { place, .. } => format!("Borrow({})", place_kind_name(p_alias(place))),
        K::BorrowOfValue { value, .. } => format!("BorrowOfValue->{}", first_unsupported_expr(value)),
        K::Builtin { name, .. } => format!("Builtin({})", name),
        K::Tuple(elems) => {
            for el in elems {
                if !mono_supports_expr(el) {
                    return format!("Tuple->{}", first_unsupported_expr(el));
                }
            }
            "Tuple".to_string()
        }
        K::Call { args, .. } => {
            for a in args {
                if !mono_supports_expr(a) {
                    return format!("Call->{}", first_unsupported_expr(a));
                }
            }
            "Call".to_string()
        }
        K::MethodCall { recv, args, .. } => {
            if !mono_supports_expr(recv) {
                return format!("MethodCall.recv->{}", first_unsupported_expr(recv));
            }
            for a in args {
                if !mono_supports_expr(a) {
                    return format!("MethodCall.arg->{}", first_unsupported_expr(a));
                }
            }
            "MethodCall".to_string()
        }
        K::StructLit { fields, .. } => {
            for f in fields {
                if !mono_supports_expr(f) {
                    return format!("StructLit->{}", first_unsupported_expr(f));
                }
            }
            "StructLit".to_string()
        }
        K::VariantConstruct { payload, .. } => {
            for p in payload {
                if !mono_supports_expr(p) {
                    return format!("VariantConstruct->{}", first_unsupported_expr(p));
                }
            }
            "VariantConstruct".to_string()
        }
        K::Match { scrutinee, arms } => {
            if !mono_supports_expr(scrutinee) {
                return format!("Match.scrut->{}", first_unsupported_expr(scrutinee));
            }
            for arm in arms {
                if pattern_uses_ref_binding(&arm.pattern) {
                    return "Match(ref pattern)".to_string();
                }
                if let Some(g) = &arm.guard {
                    if !mono_supports_expr(g) {
                        return format!("Match.guard->{}", first_unsupported_expr(g));
                    }
                }
                if !mono_supports_expr(&arm.body) {
                    return format!("Match.body->{}", first_unsupported_expr(&arm.body));
                }
            }
            "Match".to_string()
        }
        K::Cast { inner, .. } => format!("Cast->{}", first_unsupported_expr(inner)),
        K::Block(b) | K::Unsafe(b) => format!("Block->{}", first_unsupported_block(b.as_ref())),
        K::Loop { body, .. } => format!("Loop->{}", first_unsupported_block(body.as_ref())),
        K::Break { value, .. } => {
            if value.is_some() { "Break(value)".to_string() } else { "Break".to_string() }
        }
        K::Continue { .. } => "Continue".to_string(),
        K::Return { .. } => "Return".to_string(),
        K::MacroCall { name, .. } => format!("MacroCall({})", name),
    }
}

fn p_alias(p: &crate::mono::MonoPlace) -> &crate::mono::MonoPlace { p }

fn mono_supports_block(b: &crate::mono::MonoBlock) -> bool {
    let mut i = 0;
    while i < b.stmts.len() {
        if !mono_supports_stmt(&b.stmts[i]) {
            return false;
        }
        i += 1;
    }
    match &b.tail {
        Some(t) => mono_supports_expr(t),
        None => true,
    }
}

fn mono_supports_stmt(s: &crate::mono::MonoStmt) -> bool {
    match s {
        crate::mono::MonoStmt::Expr(e) => mono_supports_expr(e),
        crate::mono::MonoStmt::Let { value, .. } => mono_supports_expr(value),
        crate::mono::MonoStmt::Assign { place, value, .. } => {
            mono_supports_assign_place(place) && mono_supports_expr(value)
        }
        crate::mono::MonoStmt::LetPattern { pattern, value, else_block, .. } => {
            // Two shapes:
            //   - Irrefutable pattern, no else: plain destructure (tuple,
            //     `let (a, b) = e;`).
            //   - Refutable pattern + else: `let Some(x) = e else { … };`
            //     codegen wraps a Block(Block(pattern; Br 1)); else;
            //     Unreachable; End so the success path falls through
            //     with bindings in scope.
            if !mono_supports_expr(value) {
                return false;
            }
            match else_block {
                None => irrefutable_pattern(pattern),
                Some(eb) => mono_supports_block(eb.as_ref()),
            }
        }
        _ => false,
    }
}

fn irrefutable_pattern(pat: &crate::ast::Pattern) -> bool {
    use crate::ast::PatternKind;
    match &pat.kind {
        PatternKind::Wildcard
        | PatternKind::Binding { .. } => true,
        PatternKind::At { inner, .. } => irrefutable_pattern(inner),
        PatternKind::Tuple(elems) => elems.iter().all(irrefutable_pattern),
        PatternKind::Ref { inner, .. } => irrefutable_pattern(inner),
        // Variants/lits/ranges are refutable; struct patterns *might*
        // be irrefutable (only one variant) but we conservatively
        // reject. or-patterns are refutable.
        _ => false,
    }
}

// Place support specific to assignment LHS. Same conditions as
// `mono_supports_place` — codegen_mono_assign now handles all three
// underlying shapes (Local, Memory/MemoryAt-rooted chain via address
// path, Storage::Local-rooted chain via flat-offset LocalSet path).
fn mono_supports_assign_place(p: &crate::mono::MonoPlace) -> bool {
    mono_supports_place(p)
}

fn mono_supports_expr(e: &crate::mono::MonoExpr) -> bool {
    use crate::mono::MonoExprKind as K;
    match &e.kind {
        K::Lit(_) => true,
        K::Local(_, _) => true,
        K::Block(b) | K::Unsafe(b) => mono_supports_block(b.as_ref()),
        K::Builtin { name, args, .. } => {
            if !mono_supports_builtin(name) {
                return false;
            }
            let mut i = 0;
            while i < args.len() {
                if !mono_supports_expr(&args[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        K::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                if !mono_supports_expr(&elems[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        K::Call { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                if !mono_supports_expr(&args[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        K::MethodCall { recv, args, .. } => {
            if !mono_supports_expr(recv) {
                return false;
            }
            let mut i = 0;
            while i < args.len() {
                if !mono_supports_expr(&args[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        K::Borrow { place, .. } => mono_supports_place(place),
        K::BorrowOfValue { value, .. } => mono_supports_expr(value),
        K::PlaceLoad(p) => mono_supports_place(p),
        K::StructLit { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                if !mono_supports_expr(&fields[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        K::Cast { inner, .. } => mono_supports_expr(inner),
        K::VariantConstruct { payload, .. } => {
            let mut i = 0;
            while i < payload.len() {
                if !mono_supports_expr(&payload[i]) {
                    return false;
                }
                i += 1;
            }
            true
        }
        K::Match { scrutinee, arms } => {
            if !mono_supports_expr(scrutinee) {
                return false;
            }
            let mut i = 0;
            while i < arms.len() {
                let arm = &arms[i];
                if arm.guard.is_some() {
                    if let Some(g) = &arm.guard {
                        if !mono_supports_expr(g) {
                            return false;
                        }
                    }
                }
                if !mono_supports_expr(&arm.body) {
                    return false;
                }
                i += 1;
            }
            true
        }
        // Loop with no break-with-value (the body's tail must be
        // unit-typed). The post-lowering loop result type is `()` for
        // the lowered while/for shape; `loop { break X }` (which yields
        // X) needs a multi-valued BlockType and isn't supported yet.
        K::Loop { body, .. } => {
            // Body's stmts and tail (if any) must all be Mono-supported.
            mono_supports_block(body.as_ref())
        }
        // Break / Continue: only no-value forms (typed `!`). `break X`
        // pairs with a value-producing loop, deferred.
        K::Break { value, .. } => value.is_none(),
        K::Continue { .. } => true,
        K::Return { value } => match value {
            Some(v) => mono_supports_expr(v),
            None => true,
        },
        K::MacroCall { name, args } => name == "panic" && args.iter().all(mono_supports_expr),
        _ => false,
    }
}

fn mono_supports_place(p: &crate::mono::MonoPlace) -> bool {
    use crate::mono::MonoPlaceKind as PK;
    match &p.kind {
        PK::Local(_) => true,
        PK::Field { base, .. } | PK::TupleIndex { base, .. } => mono_supports_place(base),
        // Deref's inner evaluates to the address — codegen
        // (`emit_place_address`'s Deref arm) lowers `inner` and adds
        // any accumulated field offset. That works iff `inner`'s value
        // IS an address: i.e., the inner has type `&T` / `*T` (a single
        // i32). For value-typed inners (e.g. a Call returning a struct
        // value), the inner pushes flat scalars instead, and the chain
        // miscompiles. Gate on inner type being Ref/RawPtr — anything
        // single-i32 — AND on the inner expr being supported by Mono.
        PK::Deref { inner } => {
            if !matches!(&inner.ty, RType::Ref { .. } | RType::RawPtr { .. }) {
                return false;
            }
            mono_supports_expr(inner)
        }
    }
}

fn codegen_mono_block(
    ctx: &mut FnCtx,
    block: &crate::mono::MonoBlock,
) -> Result<(), Error> {
    // Function-body codegen: emit stmts + tail. No drop emission —
    // the function epilogue handles drops for ALL ctx.locals
    // (including these stmts' lets) at function end.
    let mut i = 0;
    while i < block.stmts.len() {
        codegen_mono_stmt(ctx, &block.stmts[i])?;
        i += 1;
    }
    if let Some(tail) = &block.tail {
        codegen_mono_expr(ctx, tail)?;
    }
    Ok(())
}

// Codegen for a value-producing inner Block expression (mirrors
// AST's codegen_block_expr). Saves a `mark` of ctx.locals.len() at
// entry; processes stmts (which may push to ctx.locals); processes
// tail; then emits drops for any bindings introduced [mark..end] —
// preserving the tail value across the drops by stashing it to
// fresh wasm locals. Finally truncates ctx.locals back to `mark`.
fn codegen_mono_block_expr(
    ctx: &mut FnCtx,
    block: &crate::mono::MonoBlock,
) -> Result<(), Error> {
    let mark = ctx.locals.len();
    let mut i = 0;
    while i < block.stmts.len() {
        codegen_mono_stmt(ctx, &block.stmts[i])?;
        i += 1;
    }
    let tail_ty = match &block.tail {
        Some(tail) => {
            codegen_mono_expr(ctx, tail)?;
            tail.ty.clone()
        }
        None => RType::Tuple(Vec::new()),
    };
    // Save tail value to fresh locals before emitting drops.
    let mut tail_flat: Vec<wasm::ValType> = Vec::new();
    flatten_rtype(&tail_ty, ctx.structs, &mut tail_flat);
    if !tail_flat.is_empty() {
        let save_start = ctx.next_wasm_local;
        let mut k = 0;
        while k < tail_flat.len() {
            ctx.extra_locals.push(tail_flat[k].copy());
            ctx.next_wasm_local += 1;
            k += 1;
        }
        let mut k = tail_flat.len();
        while k > 0 {
            k -= 1;
            ctx.instructions
                .push(wasm::Instruction::LocalSet(save_start + k as u32));
        }
        emit_drops_for_locals_range(ctx, mark, ctx.locals.len())?;
        let mut k = 0;
        while k < tail_flat.len() {
            ctx.instructions
                .push(wasm::Instruction::LocalGet(save_start + k as u32));
            k += 1;
        }
    } else {
        emit_drops_for_locals_range(ctx, mark, ctx.locals.len())?;
    }
    ctx.locals.truncate(mark);
    Ok(())
}

fn codegen_mono_stmt(
    ctx: &mut FnCtx,
    stmt: &crate::mono::MonoStmt,
) -> Result<(), Error> {
    match stmt {
        crate::mono::MonoStmt::Expr(e) => {
            codegen_mono_expr(ctx, e)?;
            // Statement-position expr: drop its value if any.
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&e.ty, ctx.structs, &mut vts);
            let mut k = 0;
            while k < vts.len() {
                ctx.instructions.push(wasm::Instruction::Drop);
                k += 1;
            }
            Ok(())
        }
        crate::mono::MonoStmt::Let { binding, value, .. } => {
            // Look up the binding's MonoLocal info + pre-computed
            // storage decision (BindingId-keyed). Synthesized bindings
            // (for-loop `__iter`, try-op arm bodies) get MemoryAt:
            // codegen allocates a dynamic shadow-stack slot here. Plain
            // `let x = e` bindings get Memory or Local from the layout
            // pass's escape analysis. Pattern leaves never appear as
            // MonoStmt::Let (codegen_pattern at match-arm time handles
            // those via bind_pattern_value).
            let mono_body = ctx.mono_body
                .expect("Mono codegen invoked without ctx.mono_body set");
            let local = &mono_body.locals[*binding as usize];
            let storage_kind = ctx.binding_storage[*binding as usize];
            let name = local.name.clone();
            let ty = local.ty.clone();
            // Compute drop_action for the binding.
            let drop_action = crate::layout::compute_drop_action(
                &name,
                &ty,
                &ctx.moved_places,
                ctx.structs,
                ctx.enums,
                ctx.traits,
            );
            // Emit the value (pushes flat scalars).
            codegen_mono_expr(ctx, value)?;
            // Store into the binding's storage and push LocalBinding.
            // Record BindingId → ctx.locals position so subsequent
            // Local(binding_id) lookups resolve correctly.
            match storage_kind {
                BindingStorageKind::Memory { frame_offset } => {
                    store_flat_to_memory(ctx, &ty, BaseAddr::StackPointer, frame_offset);
                    ctx.locals.push(LocalBinding {
                        name: name.clone(),
                        rtype: ty.clone(),
                        storage: Storage::Memory { frame_offset },
                        drop_action,
                    });
                }
                BindingStorageKind::Local => {
                    let mut vts: Vec<wasm::ValType> = Vec::new();
                    flatten_rtype(&ty, ctx.structs, &mut vts);
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
                        name: name.clone(),
                        rtype: ty.clone(),
                        storage: Storage::Local { wasm_start: start, flat_size },
                        drop_action,
                    });
                }
                BindingStorageKind::MemoryAt => {
                    // Dynamic shadow-stack allocation: __sp -= bytes,
                    // cache addr_local = __sp, store the value's flat
                    // scalars (still on the wasm stack) into the slot.
                    // Synthesized bindings (for-loop's __iter, try-op
                    // arm bodies) and any addressed binding the layout
                    // pass marked MemoryAt land here.
                    let bytes = byte_size_of(&ty, ctx.structs, ctx.enums);
                    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
                    ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
                    ctx.instructions.push(wasm::Instruction::I32Sub);
                    ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
                    let addr_local = ctx.next_wasm_local;
                    ctx.extra_locals.push(wasm::ValType::I32);
                    ctx.next_wasm_local += 1;
                    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
                    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
                    store_flat_to_memory(ctx, &ty, BaseAddr::WasmLocal(addr_local), 0);
                    ctx.locals.push(LocalBinding {
                        name: name.clone(),
                        rtype: ty.clone(),
                        storage: Storage::MemoryAt { addr_local },
                        drop_action,
                    });
                }
            }
            ctx.mono_binding_to_local[*binding as usize] = Some(ctx.locals.len() as u32 - 1);
            // Drop flag allocation for Flagged bindings.
            if matches!(drop_action, crate::layout::DropAction::Flagged) {
                let flag_idx = ctx.next_wasm_local;
                ctx.extra_locals.push(wasm::ValType::I32);
                ctx.next_wasm_local += 1;
                ctx.drop_flags.push((name, flag_idx));
                ctx.instructions.push(wasm::Instruction::I32Const(1));
                ctx.instructions.push(wasm::Instruction::LocalSet(flag_idx));
            }
            Ok(())
        }
        crate::mono::MonoStmt::Assign { place, value, .. } => {
            codegen_mono_assign(ctx, place, value)
        }
        crate::mono::MonoStmt::LetPattern { pattern, value, else_block, .. } => {
            // Codegen value (pushes flat scalars), stash to wasm
            // locals for stable read, then reuse codegen_pattern to
            // bind each leaf into ctx.locals. Map BindingIds (allocated
            // by lowering's declare_pattern_bindings) to ctx.locals
            // positions so subsequent Local(BindingId) references
            // resolve.
            codegen_mono_expr(ctx, value)?;
            let value_ty = value.ty.clone();
            // For ref bindings (`let &x = …` / `let Some(ref x) = …
            // else { … }`), the binding takes the scrutinee's address —
            // requires a stable Memory storage. For value bindings,
            // wasm-locals stash is enough.
            let is_enum_scrut = matches!(&value_ty, RType::Enum { .. });
            let needs_ref_spill = !is_enum_scrut && pattern_uses_ref_binding(pattern);
            let storage = if needs_ref_spill {
                spill_match_scrutinee(ctx, &value_ty)
            } else {
                stash_match_scrutinee(ctx, &value_ty)
            };
            let mark = ctx.locals.len();
            match else_block {
                None => {
                    // Irrefutable: no_match_target unused.
                    codegen_pattern(ctx, pattern, &value_ty, &storage, 0)?;
                    let mut next_pos: usize = mark;
                    map_arm_pattern_bindings(ctx, pattern, &mut next_pos);
                }
                Some(eb) => {
                    // Refutable + diverging else, mirroring AST
                    // codegen_let_else:
                    //   Block (outer / success target)
                    //     Block (inner / no-match target)
                    //       <pattern test>      ; on no-match, Br 0 to inner end
                    //       Br 1                ; success → outer end
                    //     End                   ; close inner
                    //     <else block>          ; diverges
                    //     Unreachable
                    //   End                     ; close outer; bindings live
                    ctx.instructions.push(wasm::Instruction::Block(wasm::BlockType::Empty));
                    ctx.instructions.push(wasm::Instruction::Block(wasm::BlockType::Empty));
                    codegen_pattern(ctx, pattern, &value_ty, &storage, 0)?;
                    ctx.instructions.push(wasm::Instruction::Br(1));
                    ctx.instructions.push(wasm::Instruction::End); // close inner
                    // Else block runs and must diverge (typeck-verified).
                    // Emit it as a value-producing block expr; the value
                    // (a `!`-typed result, no scalars) is discarded.
                    codegen_mono_block_expr(ctx, eb.as_ref())?;
                    ctx.instructions.push(wasm::Instruction::Unreachable);
                    ctx.instructions.push(wasm::Instruction::End); // close outer
                    let mut next_pos: usize = mark;
                    map_arm_pattern_bindings(ctx, pattern, &mut next_pos);
                }
            }
            Ok(())
        }
        _ => Err(Error {
            file: String::new(),
            message: "codegen_mono_stmt: variant not yet supported".to_string(),
            span: crate::span::Span::new(
                crate::span::Pos::new(1, 1),
                crate::span::Pos::new(1, 1),
            ),
        }),
    }
}

// Assign a value to a MonoPlace. Three paths:
//   1. Local-rooted with Storage::Local: write directly via LocalSet.
//   2. Field/TupleIndex chain bottoming at Local with Storage::Local:
//      compute flat-scalar offset within the binding, write via LocalSet
//      to that range.
//   3. Anything else: get the place's address into a wasm local, then
//      store_flat_to_memory.
fn codegen_mono_assign(
    ctx: &mut FnCtx,
    place: &crate::mono::MonoPlace,
    value: &crate::mono::MonoExpr,
) -> Result<(), Error> {
    use crate::mono::MonoPlaceKind as PK;
    // Path 1: simple Local Storage::Local.
    if let PK::Local(id) = &place.kind {
        let local_idx = ctx.mono_binding_to_local[*id as usize]
            .expect("BindingId without ctx.locals mapping") as usize;
        if let Storage::Local { wasm_start, flat_size } = ctx.locals[local_idx].storage {
            codegen_mono_expr(ctx, value)?;
            let mut k = 0;
            while k < flat_size {
                ctx.instructions
                    .push(wasm::Instruction::LocalSet(wasm_start + flat_size - 1 - k));
                k += 1;
            }
            return Ok(());
        }
    }
    // Path 2: Field/TupleIndex chain bottoming at Local Storage::Local.
    if let Some((wasm_start, flat_off)) = mono_place_to_local_storage_offset(ctx, place) {
        let mut vts: Vec<wasm::ValType> = Vec::new();
        flatten_rtype(&place.ty, ctx.structs, &mut vts);
        let flat_size = vts.len() as u32;
        codegen_mono_expr(ctx, value)?;
        let start = wasm_start + flat_off;
        let mut k = 0;
        while k < flat_size {
            ctx.instructions
                .push(wasm::Instruction::LocalSet(start + flat_size - 1 - k));
            k += 1;
        }
        return Ok(());
    }
    // Path 3: general — stash address, store_flat_to_memory.
    emit_place_address(ctx, place);
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    codegen_mono_expr(ctx, value)?;
    store_flat_to_memory(ctx, &place.ty, BaseAddr::WasmLocal(addr_local), 0);
    Ok(())
}

// If `place` is a Field/TupleIndex chain bottoming at a Local-rooted
// binding with Storage::Local, return `(wasm_start, flat_offset)`
// where `flat_offset` is the flat-scalar offset of the chain's leaf
// within the binding's flat representation. Returns None for any
// other shape (Local-only, chain bottoming at Local Memory/MemoryAt,
// chain bottoming at Deref).
fn mono_place_to_local_storage_offset(
    ctx: &FnCtx,
    place: &crate::mono::MonoPlace,
) -> Option<(u32, u32)> {
    use crate::mono::MonoPlaceKind as PK;
    // Reject if not a Field/TupleIndex chain.
    if !matches!(&place.kind, PK::Field { .. } | PK::TupleIndex { .. }) {
        return None;
    }
    // Build chain of names + find root binding.
    let mut chain_rev: Vec<String> = Vec::new();
    let mut p = place;
    loop {
        match &p.kind {
            PK::Local(id) => {
                let local_idx = ctx.mono_binding_to_local[*id as usize]? as usize;
                let wasm_start = match &ctx.locals[local_idx].storage {
                    Storage::Local { wasm_start, .. } => *wasm_start,
                    _ => return None,
                };
                chain_rev.push(ctx.locals[local_idx].name.clone());
                let mut chain: Vec<String> = chain_rev.into_iter().rev().collect();
                // Move chain[0] (the leaf segment) to the start; we
                // collected leaf-first then reversed, so chain is now
                // root-first. flat_chain_offset wants [root, ..., leaf].
                // Wait — our collection was [field2, field1, root];
                // reversed gives [root, field1, field2]. That matches
                // flat_chain_offset's expectation.
                let _ = &mut chain; // silence "unused mut" if any
                let flat_off = flat_chain_offset(ctx, &chain, local_idx);
                return Some((wasm_start, flat_off));
            }
            PK::Field { base, field_name, .. } => {
                chain_rev.push(field_name.clone());
                p = base;
            }
            PK::TupleIndex { base, index, .. } => {
                chain_rev.push(format!("{}", index));
                p = base;
            }
            PK::Deref { .. } => return None,
        }
    }
}

fn codegen_mono_expr(
    ctx: &mut FnCtx,
    expr: &crate::mono::MonoExpr,
) -> Result<(), Error> {
    use crate::mono::MonoExprKind as K;
    match &expr.kind {
        K::Lit(lit) => codegen_mono_lit(ctx, lit, &expr.ty),
        K::Local(binding_id, src_node_id) => {
            // Look up via the BindingId → ctx.locals map. The map is
            // populated for params at function entry and updated by
            // Let codegen as bindings come into scope.
            let local_idx = match ctx.mono_binding_to_local
                .get(*binding_id as usize)
                .and_then(|o| *o)
            {
                Some(idx) => idx as usize,
                None => return Err(Error {
                    file: String::new(),
                    message: format!(
                        "codegen_mono_expr: no ctx.locals mapping for BindingId {}",
                        binding_id
                    ),
                    span: expr.span.copy(),
                }),
            };
            emit_local_value_load(ctx, local_idx);
            // If borrowck recorded a whole-binding move at this read
            // site (`MaybeMoved` semantics), clear the binding's drop
            // flag so the scope-end drop is skipped on this path. Synth
            // Locals (try-op arm bodies, etc.) carry `src_node_id =
            // u32::MAX` and never match a real move site.
            if *src_node_id != u32::MAX {
                let name = ctx.locals[local_idx].name.clone();
                if is_move_site(&ctx.move_sites, *src_node_id, &name) {
                    if let Some(flag_idx) = lookup_drop_flag(&ctx.drop_flags, &name) {
                        ctx.instructions.push(wasm::Instruction::I32Const(0));
                        ctx.instructions.push(wasm::Instruction::LocalSet(flag_idx));
                    }
                }
            }
            Ok(())
        }
        K::Block(b) | K::Unsafe(b) => codegen_mono_block_expr(ctx, b.as_ref()),
        K::Builtin { name, type_args, args } => {
            // Lower args first (each pushes its value onto wasm stack).
            let mut i = 0;
            while i < args.len() {
                codegen_mono_expr(ctx, &args[i])?;
                i += 1;
            }
            emit_simple_builtin(ctx, name, type_args, &expr.ty)
        }
        K::Tuple(elems) => {
            // Empty tuple → unit value (no output). Non-empty: push
            // each element's value in order; the tuple's flat
            // representation is the concatenation of element
            // representations.
            let mut i = 0;
            while i < elems.len() {
                codegen_mono_expr(ctx, &elems[i])?;
                i += 1;
            }
            Ok(())
        }
        K::Call { wasm_idx, args } => {
            // sret allocation for enum-returning callees.
            let returns_enum = matches!(&expr.ty, RType::Enum { .. });
            if returns_enum {
                emit_sret_alloc(ctx, &expr.ty);
            }
            let mut i = 0;
            while i < args.len() {
                codegen_mono_expr(ctx, &args[i])?;
                i += 1;
            }
            ctx.instructions.push(wasm::Instruction::Call(*wasm_idx));
            Ok(())
        }
        K::MethodCall { wasm_idx, recv_adjust, recv, args } => {
            let returns_enum = matches!(&expr.ty, RType::Enum { .. });
            if returns_enum {
                emit_sret_alloc(ctx, &expr.ty);
            }
            // Receiver: codegen the recv expression, possibly with implicit
            // borrow per recv_adjust. For Move and ByRef, pass through.
            // For BorrowImm/BorrowMut: prefer to push the recv's place
            // address. If the recv isn't a place expression and the
            // borrow is BorrowImm (read-only), materialize the value
            // into a fresh shadow-stack slot and push its address.
            // BorrowMut requires the recv to be a real place — typeck
            // rejects `&mut value-expr` so this case shouldn't arise.
            match recv_adjust {
                ReceiverAdjust::Move | ReceiverAdjust::ByRef => {
                    codegen_mono_expr(ctx, recv)?;
                }
                ReceiverAdjust::BorrowImm => {
                    let mut scratch: Option<crate::mono::MonoPlace> = None;
                    match mono_expr_as_place(recv, &mut scratch) {
                        Ok(place) => emit_place_address(ctx, place),
                        Err(_) => emit_materialize_as_address(ctx, recv)?,
                    }
                }
                ReceiverAdjust::BorrowMut => {
                    let mut scratch: Option<crate::mono::MonoPlace> = None;
                    let place = mono_expr_as_place(recv, &mut scratch)?;
                    emit_place_address(ctx, place);
                }
            }
            let mut i = 0;
            while i < args.len() {
                codegen_mono_expr(ctx, &args[i])?;
                i += 1;
            }
            ctx.instructions.push(wasm::Instruction::Call(*wasm_idx));
            Ok(())
        }
        K::Borrow { place, .. } => {
            emit_place_address(ctx, place);
            Ok(())
        }
        K::BorrowOfValue { value, .. } => {
            // Materialize the value into a fresh shadow-stack slot,
            // push the slot's address. Mirrors codegen_borrow's
            // "non-place inner" path.
            codegen_mono_expr(ctx, value)?;
            let bytes = byte_size_of(&value.ty, ctx.structs, ctx.enums);
            // __sp -= bytes
            ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
            ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
            ctx.instructions.push(wasm::Instruction::I32Sub);
            ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
            // Cache the slot's address.
            let addr_local = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
            ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
            // Store value (already on stack) into the slot.
            store_flat_to_memory(ctx, &value.ty, BaseAddr::WasmLocal(addr_local), 0);
            // Push the slot's address as the borrow's result.
            ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
            Ok(())
        }
        K::PlaceLoad(place) => {
            codegen_mono_place_load(ctx, place);
            Ok(())
        }
        K::StructLit { fields, .. } => {
            // Emit each field's value in declared order. The struct's
            // flat representation is the concatenation of field
            // representations, so codegenning each field's value pushes
            // the right scalars in the right order.
            let mut i = 0;
            while i < fields.len() {
                codegen_mono_expr(ctx, &fields[i])?;
                i += 1;
            }
            Ok(())
        }
        K::VariantConstruct { enum_path, type_args, disc, payload } => {
            codegen_mono_variant_construct(ctx, enum_path, type_args, *disc, payload, &expr.ty)
        }
        K::Match { scrutinee, arms } => {
            codegen_mono_match(ctx, scrutinee, arms, &expr.ty)
        }
        K::Cast { inner, target } => {
            let src_ty = inner.ty.clone();
            codegen_mono_expr(ctx, inner)?;
            // Mirror codegen_expr's Cast handling: integer/char
            // conversions emit wasm conversion ops; raw-ptr → int and
            // vice versa share the i32 representation but route through
            // emit_int_to_int_cast for width changes.
            match (&src_ty, target) {
                (RType::Int(src_k), RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, src_k, tgt_k);
                }
                (RType::RawPtr { .. }, RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, &IntKind::Usize, tgt_k);
                }
                (RType::Char, RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, &IntKind::U32, tgt_k);
                }
                (RType::Int(src_k), RType::Char) => {
                    emit_int_to_int_cast(ctx, src_k, &IntKind::U32);
                }
                (RType::Int(src_k), RType::RawPtr { .. }) => {
                    emit_int_to_int_cast(ctx, src_k, &IntKind::Usize);
                }
                (RType::Bool, RType::Int(tgt_k)) => {
                    emit_int_to_int_cast(ctx, &IntKind::U32, tgt_k);
                }
                (RType::RawPtr { .. }, RType::RawPtr { .. })
                | (RType::Ref { .. }, RType::RawPtr { .. })
                | (RType::Ref { .. }, RType::Ref { .. }) => {
                    // All flatten to i32; no wasm conversion needed.
                }
                _ => return Err(Error {
                    file: String::new(),
                    message: "codegen_mono_expr: unsupported Cast source/target combination".to_string(),
                    span: expr.span.copy(),
                }),
            }
            Ok(())
        }
        K::Loop { label, body } => {
            // Mirrors codegen_while_expr's Block(Loop(...)) shape, but
            // without the cond/eqz/BrIf prefix (a `loop` runs forever
            // until break). Result type is `()` — `loop { break X; }`
            // (value-producing) needs multi-valued BlockType and is
            // gated out by mono_supports_expr.
            let outer_depth = current_cf_depth(ctx);
            let locals_at_entry = ctx.locals.len();
            ctx.instructions.push(wasm::Instruction::Block(wasm::BlockType::Empty));
            ctx.instructions.push(wasm::Instruction::Loop(wasm::BlockType::Empty));
            ctx.loops.push(LoopCgFrame {
                label: label.clone(),
                break_depth: outer_depth,
                continue_depth: outer_depth + 1,
                locals_len_at_entry: locals_at_entry,
            });
            // Run body as a block-expr so per-iteration locals get
            // dropped at the end of each iteration.
            codegen_mono_block_expr(ctx, body.as_ref())?;
            ctx.loops.pop();
            ctx.instructions.push(wasm::Instruction::Br(0));
            ctx.instructions.push(wasm::Instruction::End); // close Loop
            ctx.instructions.push(wasm::Instruction::End); // close Block
            Ok(())
        }
        K::Break { label, value } => {
            if value.is_some() {
                return Err(Error {
                    file: String::new(),
                    message: "codegen_mono_expr: break-with-value not yet supported".to_string(),
                    span: expr.span.copy(),
                });
            }
            let (frame_idx, locals_at_entry) =
                find_loop_frame(ctx, label.as_deref())
                    .expect("typeck verified break has a target");
            let break_depth = ctx.loops[frame_idx].break_depth;
            emit_drops_for_locals_range(ctx, locals_at_entry, ctx.locals.len())?;
            let cur = current_cf_depth(ctx);
            let br_idx = cur.saturating_sub(break_depth + 1);
            ctx.instructions.push(wasm::Instruction::Br(br_idx));
            Ok(())
        }
        K::Continue { label } => {
            let (frame_idx, locals_at_entry) =
                find_loop_frame(ctx, label.as_deref())
                    .expect("typeck verified continue has a target");
            let continue_depth = ctx.loops[frame_idx].continue_depth;
            emit_drops_for_locals_range(ctx, locals_at_entry, ctx.locals.len())?;
            let cur = current_cf_depth(ctx);
            let br_idx = cur.saturating_sub(continue_depth + 1);
            ctx.instructions.push(wasm::Instruction::Br(br_idx));
            Ok(())
        }
        // `return EXPR;` / `return;`. Mirrors AST `codegen_return`:
        //   1. codegen the value (or skip for unit)
        //   2. stash flat scalars into fresh wasm locals (so drops don't
        //      disturb the value)
        //   3. drop every in-scope binding (whole `ctx.locals`)
        //   4. for sret: memcpy bytes to the sret slot, push sret_ptr;
        //      otherwise push the stashed scalars back
        //   5. restore SP from `fn_entry_sp_local`
        //   6. emit wasm `Return`
        K::Return { value } => {
            if let Some(v) = value {
                codegen_mono_expr(ctx, v)?;
            }
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
            let n = ctx.locals.len();
            emit_drops_for_locals_range(ctx, 0, n)?;
            let returns_enum = matches!(
                &ctx.return_rt,
                Some(RType::Enum { .. }),
            );
            if returns_enum {
                let return_rt = ctx.return_rt.clone().expect("return_rt set when returns_enum");
                let bytes = byte_size_of(&return_rt, ctx.structs, ctx.enums);
                let dst = ctx.sret_ptr_local
                    .expect("sret_ptr present for enum returns");
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
            ctx.instructions
                .push(wasm::Instruction::LocalGet(ctx.fn_entry_sp_local));
            ctx.instructions
                .push(wasm::Instruction::GlobalSet(SP_GLOBAL));
            ctx.instructions.push(wasm::Instruction::Return);
            Ok(())
        }
        // `panic!(msg)` — codegen `msg` (an `&str` fat ref pushes ptr,
        // len), then call the imported `env.panic` (wasm func 0), then
        // `unreachable`. The expression's "result" is `!`, so the wasm
        // validator accepts dead code that follows.
        K::MacroCall { name, args } => {
            if name != "panic" {
                return Err(Error {
                    file: String::new(),
                    message: format!("codegen_mono_expr: macro `{}!` not yet supported", name),
                    span: expr.span.copy(),
                });
            }
            codegen_mono_expr(ctx, &args[0])?;
            ctx.instructions.push(wasm::Instruction::Call(0));
            ctx.instructions.push(wasm::Instruction::Unreachable);
            Ok(())
        }
        _ => Err(Error {
            file: String::new(),
            message: "codegen_mono_expr: variant not yet supported".to_string(),
            span: expr.span.copy(),
        }),
    }
}

// Codegen a Mono Match expression. Mirrors AST's codegen_match_expr:
// outer Block carries the unified result type; for each arm an inner
// Block holds the pattern test + body. Pattern binding leaves get
// pushed to ctx.locals via codegen_pattern; we walk the pattern in
// the same order to record BindingId → ctx.locals position so the
// arm's body's `Local(BindingId)` lookups resolve.
fn codegen_mono_match(
    ctx: &mut FnCtx,
    scrutinee: &crate::mono::MonoExpr,
    arms: &Vec<crate::mono::MonoArm>,
    result_ty: &RType,
) -> Result<(), Error> {
    let scrut_ty = scrutinee.ty.clone();
    // Codegen scrutinee value onto the wasm stack.
    codegen_mono_expr(ctx, scrutinee)?;
    // Stash to wasm locals (or spill if needed for ref bindings — we
    // currently reject those in mono_supports, so stash suffices).
    let is_enum_scrut = matches!(&scrut_ty, RType::Enum { .. });
    let needs_ref_spill = !is_enum_scrut
        && arms.iter().any(|a| pattern_uses_ref_binding(&a.pattern));
    let storage = if needs_ref_spill {
        spill_match_scrutinee(ctx, &scrut_ty)
    } else {
        stash_match_scrutinee(ctx, &scrut_ty)
    };
    // Outer Block: result_ty determines block_type for unified arm result.
    let bt = block_type_for(ctx, result_ty);
    ctx.instructions.push(wasm::Instruction::Block(bt));
    let mut i = 0;
    while i < arms.len() {
        let arm = &arms[i];
        ctx.instructions.push(wasm::Instruction::Block(wasm::BlockType::Empty));
        let mark = ctx.locals.len();
        // codegen_pattern emits the test (BrIf 0 to no-match) and
        // pushes pattern bindings to ctx.locals[mark..].
        codegen_pattern(ctx, &arm.pattern, &scrut_ty, &storage, 0)?;
        // Map BindingIds (allocated by lowering's
        // declare_pattern_bindings) to ctx.locals positions in the
        // same walk order.
        let mut next_pos: usize = mark;
        map_arm_pattern_bindings(ctx, &arm.pattern, &mut next_pos);
        // Optional guard.
        if let Some(g) = &arm.guard {
            codegen_mono_expr(ctx, g)?;
            ctx.instructions.push(wasm::Instruction::I32Eqz);
            ctx.instructions.push(wasm::Instruction::BrIf(0));
        }
        codegen_mono_expr(ctx, &arm.body)?;
        ctx.instructions.push(wasm::Instruction::Br(1));
        ctx.instructions.push(wasm::Instruction::End);
        ctx.locals.truncate(mark);
        i += 1;
    }
    ctx.instructions.push(wasm::Instruction::Unreachable);
    ctx.instructions.push(wasm::Instruction::End);
    Ok(())
}

// Walk an AstPattern in declare_pattern_bindings' order; for each
// Binding/At leaf, find its BindingId in mono_body.locals (matched by
// origin = Pattern(pat.id)) and record the mapping to ctx.locals[pos].
// Increments `pos` for each binding leaf encountered.
fn map_arm_pattern_bindings(
    ctx: &mut FnCtx,
    pat: &crate::ast::Pattern,
    pos: &mut usize,
) {
    use crate::ast::PatternKind;
    match &pat.kind {
        PatternKind::Binding { name, .. } | PatternKind::At { name, .. } => {
            // Look up BindingId. Normal AST patterns: match by `pat.id`
            // against `BindingOrigin::Pattern(nid)`. Synth-built patterns
            // (try-op's `Some(__ok_val_0)` / `Err(__err_0)`) carry
            // `pat.id = 0` and a `Synthesized(name)` origin; fall back
            // to matching the binding name.
            let mono_body = ctx.mono_body
                .expect("Mono codegen invoked without ctx.mono_body");
            let mut found_id: Option<u32> = None;
            let mut k = 0;
            while k < mono_body.locals.len() {
                if let crate::mono::BindingOrigin::Pattern(nid) = &mono_body.locals[k].origin {
                    if *nid == pat.id {
                        found_id = Some(k as u32);
                        break;
                    }
                }
                k += 1;
            }
            if found_id.is_none() {
                let mut k = 0;
                while k < mono_body.locals.len() {
                    if matches!(
                        &mono_body.locals[k].origin,
                        crate::mono::BindingOrigin::Synthesized(_),
                    ) && mono_body.locals[k].name == *name
                    {
                        found_id = Some(k as u32);
                        break;
                    }
                    k += 1;
                }
            }
            if let Some(bid) = found_id {
                ctx.mono_binding_to_local[bid as usize] = Some(*pos as u32);
            }
            *pos += 1;
            if let PatternKind::At { inner, .. } = &pat.kind {
                map_arm_pattern_bindings(ctx, inner, pos);
            }
        }
        PatternKind::Tuple(elems) | PatternKind::VariantTuple { elems, .. } => {
            let mut i = 0;
            while i < elems.len() {
                map_arm_pattern_bindings(ctx, &elems[i], pos);
                i += 1;
            }
        }
        PatternKind::VariantStruct { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                map_arm_pattern_bindings(ctx, &fields[i].pattern, pos);
                i += 1;
            }
        }
        PatternKind::Ref { inner, .. } => map_arm_pattern_bindings(ctx, inner, pos),
        PatternKind::Or(alts) => {
            // All alts bind the same set; walk first.
            if !alts.is_empty() {
                map_arm_pattern_bindings(ctx, &alts[0], pos);
            }
        }
        _ => {}
    }
}

// Construct an enum variant value: allocate a shadow-stack slot,
// write the discriminant + each payload field at its byte offset,
// push the slot's address as the result. Mirrors AST's
// codegen_variant_construction for tuple variants. Lowering's
// `payload` is already in declared order.
fn codegen_mono_variant_construct(
    ctx: &mut FnCtx,
    enum_path: &Vec<String>,
    type_args: &Vec<RType>,
    disc: u32,
    payload: &Vec<crate::mono::MonoExpr>,
    enum_ty: &RType,
) -> Result<(), Error> {
    let total_size = byte_size_of(enum_ty, ctx.structs, ctx.enums);
    let entry = crate::typeck::enum_lookup(ctx.enums, enum_path)
        .expect("typeck verified the enum exists");
    let env = build_env(&entry.type_params, type_args);
    let variant = &entry.variants[disc as usize];
    // Compute payload offsets + types in declared order.
    let mut payload_offsets: Vec<u32> = Vec::new();
    let mut payload_types: Vec<RType> = Vec::new();
    let mut off: u32 = 4; // disc takes 4 bytes
    match &variant.payload {
        crate::typeck::VariantPayloadResolved::Unit => {}
        crate::typeck::VariantPayloadResolved::Tuple(types_decl) => {
            let mut i = 0;
            while i < types_decl.len() {
                let ty = substitute_rtype(&types_decl[i], &env);
                payload_offsets.push(off);
                off += byte_size_of(&ty, ctx.structs, ctx.enums);
                payload_types.push(ty);
                i += 1;
            }
        }
        crate::typeck::VariantPayloadResolved::Struct(fields) => {
            let mut i = 0;
            while i < fields.len() {
                let ty = substitute_rtype(&fields[i].ty, &env);
                payload_offsets.push(off);
                off += byte_size_of(&ty, ctx.structs, ctx.enums);
                payload_types.push(ty);
                i += 1;
            }
        }
    }
    // Allocate the slot: __sp -= total_size.
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::I32Const(total_size as i32));
    ctx.instructions.push(wasm::Instruction::I32Sub);
    ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    // Cache the address.
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    // Store discriminant at offset 0.
    ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
    ctx.instructions.push(wasm::Instruction::I32Const(disc as i32));
    ctx.instructions.push(wasm::Instruction::I32Store { align: 2, offset: 0 });
    // Store each payload field at its offset.
    if payload.len() != payload_offsets.len() {
        return Err(Error {
            file: String::new(),
            message: format!(
                "codegen_mono_variant_construct: payload arity {} != declared {}",
                payload.len(), payload_offsets.len()
            ),
            span: crate::span::Span::new(crate::span::Pos::new(1, 1), crate::span::Pos::new(1, 1)),
        });
    }
    let mut i = 0;
    while i < payload.len() {
        codegen_mono_expr(ctx, &payload[i])?;
        store_flat_to_memory(ctx, &payload_types[i], BaseAddr::WasmLocal(addr_local), payload_offsets[i]);
        i += 1;
    }
    // Push the slot's address as the construction's result.
    ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
    Ok(())
}

// Materialize a value-producing MonoExpr into a fresh shadow-stack
// slot and push the slot's address as a single i32. Used by autoref
// MethodCall for non-place BorrowImm receivers.
fn emit_materialize_as_address(
    ctx: &mut FnCtx,
    expr: &crate::mono::MonoExpr,
) -> Result<(), Error> {
    codegen_mono_expr(ctx, expr)?;
    let bytes = byte_size_of(&expr.ty, ctx.structs, ctx.enums);
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
    ctx.instructions.push(wasm::Instruction::I32Sub);
    ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    let addr_local = ctx.next_wasm_local;
    ctx.extra_locals.push(wasm::ValType::I32);
    ctx.next_wasm_local += 1;
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
    store_flat_to_memory(ctx, &expr.ty, BaseAddr::WasmLocal(addr_local), 0);
    ctx.instructions.push(wasm::Instruction::LocalGet(addr_local));
    Ok(())
}

// Allocate a shadow-stack slot for an enum return value (sret) and
// push its address as the leading "sret_addr" Call argument.
fn emit_sret_alloc(ctx: &mut FnCtx, ty: &RType) {
    let bytes = byte_size_of(ty, ctx.structs, ctx.enums);
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::I32Const(bytes as i32));
    ctx.instructions.push(wasm::Instruction::I32Sub);
    ctx.instructions.push(wasm::Instruction::GlobalSet(SP_GLOBAL));
    ctx.instructions.push(wasm::Instruction::GlobalGet(SP_GLOBAL));
}

// Best-effort: convert a MonoExpr into a MonoPlace if it's a place-form
// (Local or PlaceLoad). For autoref method receivers, the recv must be
// a place; lowering produces PlaceLoad(...) for borrowed targets.
// Borrow the place inside an autoref'd receiver expression. Returns
// `Ok(&place)` for forms that codegen can address directly:
//   - `K::Local(id)`   — synthesizes a temporary `Local(id)` place
//                        (stored on the stack frame).
//   - `K::PlaceLoad(p)` — borrows the existing place.
// Any other expression kind isn't a place; the caller falls back to
// materializing the value via `emit_materialize_as_address`.
//
// Returning a borrow (rather than cloning) preserves the inner
// `MonoExpr` exactly — earlier the clone path swapped non-`Local`
// Deref-inners (e.g. `MethodCall`, the result of synth-Index lowering)
// for `Local(0)`, which silently miscompiled `arr[i].method(...)`
// shapes.
fn mono_expr_as_place<'a>(
    e: &'a crate::mono::MonoExpr,
    scratch: &'a mut Option<crate::mono::MonoPlace>,
) -> Result<&'a crate::mono::MonoPlace, Error> {
    use crate::mono::{MonoExprKind, MonoPlace, MonoPlaceKind};
    match &e.kind {
        MonoExprKind::Local(id, _) => {
            *scratch = Some(MonoPlace {
                kind: MonoPlaceKind::Local(*id),
                ty: e.ty.clone(),
                span: e.span.copy(),
            });
            Ok(scratch.as_ref().unwrap())
        }
        MonoExprKind::PlaceLoad(p) => Ok(p),
        _ => Err(Error {
            file: String::new(),
            message: "codegen_mono_expr: autoref receiver not in place form".to_string(),
            span: e.span.copy(),
        }),
    }
}

// Push the address of a MonoPlace onto the wasm stack as a single i32.
fn emit_place_address(ctx: &mut FnCtx, place: &crate::mono::MonoPlace) {
    use crate::mono::MonoPlaceKind as PK;
    let mut total_offset: u32 = 0;
    let mut p = place;
    // Walk Field/TupleIndex chain accumulating byte offset; bottom out
    // at Local (push slot/binding address) or Deref (codegen the inner
    // expr — must produce an i32 address). Auto-deref through refs /
    // smart pointers is structurally explicit in the IR via
    // MonoPlaceKind::Deref nodes inserted by lowering. No semantic
    // inference here.
    loop {
        match &p.kind {
            PK::Local(id) => {
                let idx = ctx.mono_binding_to_local[*id as usize]
                    .expect("BindingId without ctx.locals mapping") as usize;
                match &ctx.locals[idx].storage {
                    Storage::Memory { frame_offset } => {
                        let fb = ctx.frame_base_local;
                        ctx.instructions.push(wasm::Instruction::LocalGet(fb));
                        let off = *frame_offset + total_offset;
                        if off != 0 {
                            ctx.instructions.push(wasm::Instruction::I32Const(off as i32));
                            ctx.instructions.push(wasm::Instruction::I32Add);
                        }
                    }
                    Storage::MemoryAt { addr_local } => {
                        ctx.instructions.push(wasm::Instruction::LocalGet(*addr_local));
                        if total_offset != 0 {
                            ctx.instructions.push(wasm::Instruction::I32Const(total_offset as i32));
                            ctx.instructions.push(wasm::Instruction::I32Add);
                        }
                    }
                    Storage::Local { wasm_start, .. } => {
                        // For ref-typed bindings, the wasm local IS
                        // the ref pointer. For value-typed bindings,
                        // escape analysis would have promoted to
                        // Memory if `&binding.field` were taken, so
                        // Local + Field-chain shouldn't occur here.
                        ctx.instructions.push(wasm::Instruction::LocalGet(*wasm_start));
                        if total_offset != 0 {
                            ctx.instructions.push(wasm::Instruction::I32Const(total_offset as i32));
                            ctx.instructions.push(wasm::Instruction::I32Add);
                        }
                    }
                }
                return;
            }
            PK::Field { base, byte_offset, .. } => {
                total_offset += *byte_offset;
                p = base;
            }
            PK::TupleIndex { base, byte_offset, .. } => {
                total_offset += *byte_offset;
                p = base;
            }
            PK::Deref { inner } => {
                // The inner expression evaluates to an address — push
                // it directly. Then add total_offset for any wrapping
                // Field/TupleIndex.
                let _ = codegen_mono_expr(ctx, inner);
                if total_offset != 0 {
                    ctx.instructions.push(wasm::Instruction::I32Const(total_offset as i32));
                    ctx.instructions.push(wasm::Instruction::I32Add);
                }
                return;
            }
        }
    }
}

fn codegen_mono_lit(
    ctx: &mut FnCtx,
    lit: &crate::mono::MonoLit,
    ty: &RType,
) -> Result<(), Error> {
    use crate::mono::MonoLit;
    match lit {
        MonoLit::Int { magnitude, negated } => {
            // Reuse the AST emit_int_lit which handles all int widths
            // (≤32-bit, 64-bit, 128-bit two-half push).
            emit_int_lit(ctx, ty, *magnitude, *negated);
            Ok(())
        }
        MonoLit::Bool(b) => {
            ctx.instructions.push(wasm::Instruction::I32Const(if *b { 1 } else { 0 }));
            Ok(())
        }
        MonoLit::Char(c) => {
            ctx.instructions.push(wasm::Instruction::I32Const(*c as i32));
            Ok(())
        }
        MonoLit::Str(s) => {
            // Intern via the existing string pool; emit as fat ref.
            let (addr, len) = ctx.mono.intern_str(s);
            ctx.instructions.push(wasm::Instruction::I32Const(addr as i32));
            ctx.instructions.push(wasm::Instruction::I32Const(len as i32));
            Ok(())
        }
    }
}

// Push the value at a MonoPlace onto the wasm stack as flat scalars.
// Three cases:
//   - Local: just LocalGet (works for both value- and ref-typed bindings).
//   - Field/TupleIndex chain: extract a name chain and delegate to the
//     existing codegen_place_chain_load, which handles both flat-stored
//     value bindings (peel field bytes off the flat scalars) and
//     ref-rooted chains (auto-deref + load).
//   - Deref: codegen the inner expression as the address, stash, then
//     load_flat_from_memory.
fn codegen_mono_place_load(ctx: &mut FnCtx, place: &crate::mono::MonoPlace) {
    use crate::mono::MonoPlaceKind as PK;
    match &place.kind {
        PK::Local(id) => {
            let idx = ctx.mono_binding_to_local[*id as usize]
                .expect("BindingId without ctx.locals mapping") as usize;
            emit_local_value_load(ctx, idx);
        }
        PK::Field { .. } | PK::TupleIndex { .. } => {
            // Build a name chain: [root_name, field/index, …, leaf].
            let mut chain: Vec<String> = Vec::new();
            let mut p = place;
            loop {
                match &p.kind {
                    PK::Local(id) => {
                        let idx = ctx.mono_binding_to_local[*id as usize]
                            .expect("BindingId without ctx.locals mapping") as usize;
                        chain.push(ctx.locals[idx].name.clone());
                        // Reverse — we collected leaf-first, want root-first.
                        chain.reverse();
                        let _ = codegen_place_chain_load(ctx, &chain);
                        return;
                    }
                    PK::Field { base, field_name, .. } => {
                        chain.push(field_name.clone());
                        p = base;
                    }
                    PK::TupleIndex { base, index, .. } => {
                        chain.push(format!("{}", index));
                        p = base;
                    }
                    PK::Deref { .. } => {
                        // Chain rooted at a Deref — fall through to
                        // address-then-load below (rare in practice).
                        emit_place_address(ctx, place);
                        let addr_local = ctx.next_wasm_local;
                        ctx.extra_locals.push(wasm::ValType::I32);
                        ctx.next_wasm_local += 1;
                        ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
                        load_flat_from_memory(ctx, &place.ty, BaseAddr::WasmLocal(addr_local), 0);
                        return;
                    }
                }
            }
        }
        PK::Deref { .. } => {
            emit_place_address(ctx, place);
            let addr_local = ctx.next_wasm_local;
            ctx.extra_locals.push(wasm::ValType::I32);
            ctx.next_wasm_local += 1;
            ctx.instructions.push(wasm::Instruction::LocalSet(addr_local));
            load_flat_from_memory(ctx, &place.ty, BaseAddr::WasmLocal(addr_local), 0);
        }
    }
}

// Emit a load of a binding's value (no implicit deref through a ref).
// Mirrors codegen_var's load logic but without move-site clearing,
// since the Mono path doesn't currently handle move-site annotations.
fn emit_local_value_load(ctx: &mut FnCtx, idx: usize) {
    let rt = ctx.locals[idx].rtype.clone();
    match &ctx.locals[idx].storage {
        Storage::Local { wasm_start, flat_size } => {
            let start = *wasm_start;
            let n = *flat_size;
            let mut k = 0;
            while k < n {
                ctx.instructions.push(wasm::Instruction::LocalGet(start + k));
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
}

// True iff the builtin name is in the Mono-path-supported set:
// `<int_kind>_<op>` for ≤64-bit ints + bool, where the op is one of
// the simple arithmetic/comparison/bitwise ops. Excludes 128-bit ints
// (need codegen_builtin_128's multi-instruction sequences) and the
// non-arith intrinsics (alloc, free, cast, size_of, slice/str
// helpers, ptr arith). The Mono path will gain those in follow-up
// turns; for now anything else falls back to AST.
fn mono_supports_builtin(name: &str) -> bool {
    match name {
        "size_of"
        | "cast"
        | "alloc"
        | "free"
        | "slice_ptr"
        | "slice_mut_ptr"
        | "slice_len"
        | "str_len"
        | "str_as_bytes"
        | "str_as_mut_bytes"
        | "make_slice"
        | "make_mut_slice"
        | "make_str"
        | "make_mut_str"
        | "ptr_usize_add"
        | "ptr_usize_sub"
        | "ptr_isize_offset" => return true,
        _ => {}
    }
    split_builtin_name(name).is_some()
}

// Emit a wasm op for arithmetic/comparison + typed builtins (args
// already on stack). Mirrors the dispatch in codegen_builtin. Used
// only by Mono path; the AST path goes through codegen_builtin
// directly.
fn emit_simple_builtin(
    ctx: &mut FnCtx,
    name: &str,
    type_args: &Vec<RType>,
    _ty: &RType,
) -> Result<(), Error> {
    match name {
        // `¤size_of::<T>()`. Args list is empty (caller emitted nothing).
        // The size is a compile-time constant of T.
        "size_of" => {
            let t = type_args.get(0).expect("¤size_of always has T type-arg");
            let size = byte_size_of(t, ctx.structs, ctx.enums);
            ctx.instructions.push(wasm::Instruction::I32Const(size as i32));
            return Ok(());
        }
        // `¤cast::<A, B>(p)` and `¤str_as_bytes(s)` are runtime no-ops:
        // raw ptrs and (&str / &[u8]) share the same flat repr, so the
        // arg's already-on-stack scalars are the result. Same for
        // `make_slice`/`make_str` family — args already arrive as the
        // (ptr, len) pair the fat-ref expects.
        "cast"
        | "str_as_bytes"
        | "str_as_mut_bytes"
        | "make_slice"
        | "make_mut_slice"
        | "make_str"
        | "make_mut_str" => return Ok(()),
        // `¤free(p)` evaluates p (already on stack) and discards.
        "free" => {
            ctx.instructions.push(wasm::Instruction::Drop);
            return Ok(());
        }
        // `¤alloc(n)` — bump the heap pointer by n, return the old
        // value. Stack on entry: [n]. Mirrors codegen_builtin_alloc.
        "alloc" => {
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
            return Ok(());
        }
        // Fat-ref decomposition: stack on entry is [ptr, len].
        "slice_ptr" | "slice_mut_ptr" => {
            // Want ptr (below); discard len (top).
            ctx.instructions.push(wasm::Instruction::Drop);
            return Ok(());
        }
        "slice_len" | "str_len" => {
            // Want len (top); stash, drop ptr, reload.
            let len_local = alloc_i32_local(ctx);
            ctx.instructions.push(wasm::Instruction::LocalSet(len_local));
            ctx.instructions.push(wasm::Instruction::Drop);
            ctx.instructions.push(wasm::Instruction::LocalGet(len_local));
            return Ok(());
        }
        // Pointer arithmetic: stack on entry is [p, n]; both are i32.
        "ptr_usize_add" | "ptr_isize_offset" => {
            ctx.instructions.push(wasm::Instruction::I32Add);
            return Ok(());
        }
        "ptr_usize_sub" => {
            ctx.instructions.push(wasm::Instruction::I32Sub);
            return Ok(());
        }
        _ => {}
    }
    let (ty_name, op) = match split_builtin_name(name) {
        Some(p) => p,
        None => return Err(Error {
            file: String::new(),
            message: format!("emit_simple_builtin: not a `<int>_<op>` builtin: `{}`", name),
            span: crate::span::Span::new(
                crate::span::Pos::new(1, 1),
                crate::span::Pos::new(1, 1),
            ),
        }),
    };
    if matches!(ty_name, "u128" | "i128") {
        // Wide ops (u128/i128) flatten to 2 i64s per arg = 4 i64s on
        // the stack; codegen_builtin_128 pops them into temps and emits
        // the multi-instruction sequence (add-with-carry / sub-with-
        // borrow / two-half eq / signed-vs-unsigned compare). mul/div/
        // rem fall back to wasm `unreachable` inside that helper —
        // pocket-rust's bootstrap path doesn't exercise them, and the
        // proper widening multiply / long division can be added when
        // there's a caller. (Tracked in the trait-system deferral
        // ledger.)
        let signed = ty_name == "i128";
        codegen_builtin_128(ctx, op, signed);
        return Ok(());
    }
    let is_i64 = matches!(ty_name, "u64" | "i64");
    let is_signed = matches!(ty_name, "i8" | "i16" | "i32" | "i64" | "isize");
    let inst = match (is_i64, op) {
        (false, "add") => wasm::Instruction::I32Add,
        (false, "sub") => wasm::Instruction::I32Sub,
        (false, "mul") => wasm::Instruction::I32Mul,
        (false, "div") => if is_signed { wasm::Instruction::I32DivS } else { wasm::Instruction::I32DivU },
        (false, "rem") => if is_signed { wasm::Instruction::I32RemS } else { wasm::Instruction::I32RemU },
        (false, "and") => wasm::Instruction::I32And,
        (false, "or") => wasm::Instruction::I32Or,
        (false, "xor") => wasm::Instruction::I32Xor,
        (false, "eq") => wasm::Instruction::I32Eq,
        (false, "ne") => wasm::Instruction::I32Ne,
        (false, "lt") => if is_signed { wasm::Instruction::I32LtS } else { wasm::Instruction::I32LtU },
        (false, "le") => if is_signed { wasm::Instruction::I32LeS } else { wasm::Instruction::I32LeU },
        (false, "gt") => if is_signed { wasm::Instruction::I32GtS } else { wasm::Instruction::I32GtU },
        (false, "ge") => if is_signed { wasm::Instruction::I32GeS } else { wasm::Instruction::I32GeU },
        (false, "not") => wasm::Instruction::I32Eqz,
        (true, "add") => wasm::Instruction::I64Add,
        (true, "sub") => wasm::Instruction::I64Sub,
        (true, "mul") => wasm::Instruction::I64Mul,
        (true, "div") => if is_signed { wasm::Instruction::I64DivS } else { wasm::Instruction::I64DivU },
        (true, "rem") => if is_signed { wasm::Instruction::I64RemS } else { wasm::Instruction::I64RemU },
        (true, "and") => wasm::Instruction::I64And,
        (true, "or") => wasm::Instruction::I64Or,
        (true, "xor") => wasm::Instruction::I64Xor,
        (true, "eq") => wasm::Instruction::I64Eq,
        (true, "ne") => wasm::Instruction::I64Ne,
        (true, "lt") => if is_signed { wasm::Instruction::I64LtS } else { wasm::Instruction::I64LtU },
        (true, "le") => if is_signed { wasm::Instruction::I64LeS } else { wasm::Instruction::I64LeU },
        (true, "gt") => if is_signed { wasm::Instruction::I64GtS } else { wasm::Instruction::I64GtU },
        (true, "ge") => if is_signed { wasm::Instruction::I64GeS } else { wasm::Instruction::I64GeU },
        _ => return Err(Error {
            file: String::new(),
            message: format!("emit_simple_builtin: unknown op `{}_{}`", ty_name, op),
            span: crate::span::Span::new(
                crate::span::Pos::new(1, 1),
                crate::span::Pos::new(1, 1),
            ),
        }),
    };
    ctx.instructions.push(inst);
    // Narrow-int (u8/i8/u16/i16) results live in a wasm i32 wider than
    // the source type; arithmetic that can carry past the type's bit
    // width (add/sub/mul, plus signed div where i8::MIN/-1 yields 128)
    // would otherwise leak the high bits to subsequent ops. Mask back to
    // the type's representation: zero-extend for unsigned, sign-extend
    // for signed. Compares produce bool i32 (already in range), and
    // bitwise ops with in-range inputs stay in range, so neither needs a
    // fixup. For 32-bit and wider kinds the wasm representation already
    // matches the type; the helper is a no-op.
    if op_can_overflow_narrow(op, is_signed) {
        emit_narrow_width_fixup(ctx, ty_name);
    }
    Ok(())
}

fn op_can_overflow_narrow(op: &str, signed: bool) -> bool {
    matches!(op, "add" | "sub" | "mul") || (signed && op == "div")
}

fn emit_narrow_width_fixup(ctx: &mut FnCtx, ty_name: &str) {
    match ty_name {
        "u8" => {
            ctx.instructions.push(wasm::Instruction::I32Const(0xFF));
            ctx.instructions.push(wasm::Instruction::I32And);
        }
        "u16" => {
            ctx.instructions.push(wasm::Instruction::I32Const(0xFFFF));
            ctx.instructions.push(wasm::Instruction::I32And);
        }
        "i8" => {
            ctx.instructions.push(wasm::Instruction::I32Const(24));
            ctx.instructions.push(wasm::Instruction::I32Shl);
            ctx.instructions.push(wasm::Instruction::I32Const(24));
            ctx.instructions.push(wasm::Instruction::I32ShrS);
        }
        "i16" => {
            ctx.instructions.push(wasm::Instruction::I32Const(16));
            ctx.instructions.push(wasm::Instruction::I32Shl);
            ctx.instructions.push(wasm::Instruction::I32Const(16));
            ctx.instructions.push(wasm::Instruction::I32ShrS);
        }
        _ => {}
    }
}
