use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, Stmt,
    StructLit,
};
use crate::span::Error;
use crate::typeck::{
    FuncTable, IntKind, RType, StructTable, clone_path, flatten_rtype, func_lookup,
    is_ref_mutable, rtype_clone, rtype_size, struct_lookup,
};
use crate::wasm;

pub fn emit(
    wasm_mod: &mut wasm::Module,
    root: &Module,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut module_path: Vec<String> = Vec::new();
    push_root_name(&mut module_path, root);
    emit_module(wasm_mod, root, &mut module_path, structs, funcs)?;
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

struct LocalBinding {
    name: String,
    wasm_start: u32,
    size: u32,
    rtype: RType,
}

struct FnCtx<'a> {
    locals: Vec<LocalBinding>,
    next_local: u32,
    extra_locals: Vec<wasm::ValType>,
    instructions: Vec<wasm::Instruction>,
    structs: &'a StructTable,
    funcs: &'a FuncTable,
    current_module: Vec<String>,
    let_types: &'a Vec<RType>,
    lit_types: &'a Vec<RType>,
    let_idx: usize,
    lit_idx: usize,
}

fn emit_module(
    wasm_mod: &mut wasm::Module,
    module: &Module,
    path: &mut Vec<String>,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => emit_function(wasm_mod, f, path, structs, funcs)?,
            Item::Module(m) => {
                path.push(m.name.clone());
                emit_module(wasm_mod, m, path, structs, funcs)?;
                path.pop();
            }
            Item::Struct(_) => {}
        }
        i += 1;
    }
    Ok(())
}

fn emit_function(
    wasm_mod: &mut wasm::Module,
    func: &Function,
    current_module: &Vec<String>,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut full = clone_path(current_module);
    full.push(func.name.clone());
    let entry = func_lookup(funcs, &full).expect("typeck registered this function");

    let mut wasm_params: Vec<wasm::ValType> = Vec::new();
    let mut locals: Vec<LocalBinding> = Vec::new();
    let mut next_local: u32 = 0;
    let mut k = 0;
    while k < func.params.len() {
        let rt = rtype_clone(&entry.param_types[k]);
        let size = rtype_size(&rt, structs);
        let start = next_local;
        flatten_rtype(&rt, structs, &mut wasm_params);
        locals.push(LocalBinding {
            name: func.params[k].name.clone(),
            wasm_start: start,
            size,
            rtype: rt,
        });
        next_local += size;
        k += 1;
    }

    let mut wasm_results: Vec<wasm::ValType> = Vec::new();
    if let Some(rt) = &entry.return_type {
        flatten_rtype(rt, structs, &mut wasm_results);
    }
    // For each `&mut T` parameter, append T's flat ValTypes to the function's
    // results — the function returns the modified parameter values alongside
    // its normal return so the caller can store them back.
    let mut k = 0;
    while k < entry.param_types.len() {
        if is_ref_mutable(&entry.param_types[k]) {
            flatten_rtype(&entry.param_types[k], structs, &mut wasm_results);
        }
        k += 1;
    }

    let func_type = wasm::FuncType {
        params: wasm_params,
        results: wasm_results,
    };
    let type_idx = wasm_mod.types.len() as u32;
    wasm_mod.types.push(func_type);

    let func_idx = wasm_mod.functions.len() as u32;
    wasm_mod.functions.push(type_idx);

    let mut ctx = FnCtx {
        locals,
        next_local,
        extra_locals: Vec::new(),
        instructions: Vec::new(),
        structs,
        funcs,
        current_module: clone_path(current_module),
        let_types: &entry.let_types,
        lit_types: &entry.lit_types,
        let_idx: 0,
        lit_idx: 0,
    };
    codegen_block(&mut ctx, &func.body)?;

    // Emit LocalGets for each `&mut` parameter's flat locals so the values flow
    // out of the function as additional return values.
    let mut k = 0;
    while k < ctx.locals.len() && k < func.params.len() {
        if is_ref_mutable(&ctx.locals[k].rtype) {
            let start = ctx.locals[k].wasm_start;
            let size = ctx.locals[k].size;
            let mut j = 0;
            while j < size {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(start + j));
                j += 1;
            }
        }
        k += 1;
    }

    let body = wasm::FuncBody {
        locals: ctx.extra_locals,
        instructions: ctx.instructions,
    };
    wasm_mod.code.push(body);

    if current_module.is_empty() {
        wasm_mod.exports.push(wasm::Export {
            name: func.name.clone(),
            kind: wasm::ExportKind::Func,
            index: func_idx,
        });
    }

    Ok(())
}

fn codegen_block(ctx: &mut FnCtx, block: &Block) -> Result<(), Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => codegen_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => codegen_assign_stmt(ctx, assign)?,
        }
        i += 1;
    }
    if let Some(expr) = &block.tail {
        codegen_expr(ctx, expr)?;
    }
    Ok(())
}

fn codegen_assign_stmt(ctx: &mut FnCtx, assign: &AssignStmt) -> Result<(), Error> {
    // Codegen RHS first; the value sits on the stack.
    codegen_expr(ctx, &assign.rhs)?;
    // Walk the LHS chain to find the binding and the offset/size to write.
    let chain = extract_place(&assign.lhs).expect("typeck verified");
    let mut binding_idx = 0;
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
    let mut offset: u32 = 0;
    let mut current_ty = rtype_clone(&ctx.locals[binding_idx].rtype);
    let mut i = 1;
    while i < chain.len() {
        let struct_path = match &current_ty {
            RType::Struct(p) => clone_path(p),
            RType::Ref { inner, .. } => match inner.as_ref() {
                RType::Struct(p) => clone_path(p),
                _ => unreachable!("typeck verified field assignment is on a struct"),
            },
            _ => unreachable!("typeck verified field assignment is on a struct"),
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let mut field_offset: u32 = 0;
        let mut found_field = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&entry.fields[j].ty, ctx.structs, &mut vts);
            let s = vts.len() as u32;
            if entry.fields[j].name == chain[i] {
                offset += field_offset;
                current_ty = rtype_clone(&entry.fields[j].ty);
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
    let mut target_valtypes: Vec<wasm::ValType> = Vec::new();
    flatten_rtype(&current_ty, ctx.structs, &mut target_valtypes);
    let size = target_valtypes.len() as u32;
    let target_start = ctx.locals[binding_idx].wasm_start + offset;
    // LocalSet pops from the top of the stack; the rightmost scalar of the
    // value is the highest-indexed slot.
    let mut k = 0;
    while k < size {
        ctx.instructions
            .push(wasm::Instruction::LocalSet(target_start + size - 1 - k));
        k += 1;
    }
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

fn codegen_let_stmt(ctx: &mut FnCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    // `&mut T` bindings alias the source's locals — no allocation needed.
    // We must still walk the value expression so nested let_idx / lit_idx
    // counters stay in sync, but its pushed values are dropped on the floor.
    let needs_alias = is_ref_mutable(&ctx.let_types[ctx.let_idx]);
    if needs_alias {
        let alias_range = mut_ref_source_range(ctx, &let_stmt.value)
            .expect("typeck verified the &mut value is a place expression");
        codegen_expr(ctx, &let_stmt.value)?;
        let value_ty = rtype_clone(&ctx.let_types[ctx.let_idx]);
        ctx.let_idx += 1;
        let mut value_valtypes: Vec<wasm::ValType> = Vec::new();
        flatten_rtype(&value_ty, ctx.structs, &mut value_valtypes);
        let drop_count = value_valtypes.len();
        let mut k = 0;
        while k < drop_count {
            ctx.instructions.push(wasm::Instruction::Drop);
            k += 1;
        }
        let (start, size) = alias_range;
        ctx.locals.push(LocalBinding {
            name: let_stmt.name.clone(),
            wasm_start: start,
            size,
            rtype: value_ty,
        });
        return Ok(());
    }
    codegen_expr(ctx, &let_stmt.value)?;
    let value_ty = rtype_clone(&ctx.let_types[ctx.let_idx]);
    ctx.let_idx += 1;
    let mut value_valtypes: Vec<wasm::ValType> = Vec::new();
    flatten_rtype(&value_ty, ctx.structs, &mut value_valtypes);
    let size = value_valtypes.len() as u32;
    let start = ctx.next_local;
    let mut k = 0;
    while k < value_valtypes.len() {
        ctx.extra_locals.push(value_valtypes[k].copy());
        ctx.next_local += 1;
        k += 1;
    }
    let mut k = 0;
    while k < size {
        ctx.instructions
            .push(wasm::Instruction::LocalSet(start + size - 1 - k));
        k += 1;
    }
    ctx.locals.push(LocalBinding {
        name: let_stmt.name.clone(),
        wasm_start: start,
        size,
        rtype: value_ty,
    });
    Ok(())
}

// Compute the (wasm_start, size) of the underlying place that a `&mut`-typed
// value refers to. Used both for `let r = …;` aliasing and for working out
// where to write back after a call.
fn mut_ref_source_range(ctx: &FnCtx, expr: &Expr) -> Option<(u32, u32)> {
    match &expr.kind {
        ExprKind::Borrow { inner, .. } => place_range(ctx, inner.as_ref()),
        ExprKind::Var(name) => {
            let mut i = ctx.locals.len();
            while i > 0 {
                i -= 1;
                if ctx.locals[i].name == *name {
                    return Some((ctx.locals[i].wasm_start, ctx.locals[i].size));
                }
            }
            None
        }
        _ => None,
    }
}

fn place_range(ctx: &FnCtx, expr: &Expr) -> Option<(u32, u32)> {
    let chain = extract_place(expr)?;
    let mut binding_idx: Option<usize> = None;
    let mut i = ctx.locals.len();
    while i > 0 {
        i -= 1;
        if ctx.locals[i].name == chain[0] {
            binding_idx = Some(i);
            break;
        }
    }
    let bidx = binding_idx?;
    let mut offset: u32 = 0;
    let mut current_ty = rtype_clone(&ctx.locals[bidx].rtype);
    let mut i = 1;
    while i < chain.len() {
        let struct_path = match &current_ty {
            RType::Struct(p) => clone_path(p),
            RType::Ref { inner, .. } => match inner.as_ref() {
                RType::Struct(p) => clone_path(p),
                _ => return None,
            },
            _ => return None,
        };
        let entry = struct_lookup(ctx.structs, &struct_path)?;
        let mut field_offset: u32 = 0;
        let mut found = false;
        let mut j = 0;
        while j < entry.fields.len() {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&entry.fields[j].ty, ctx.structs, &mut vts);
            let s = vts.len() as u32;
            if entry.fields[j].name == chain[i] {
                offset += field_offset;
                current_ty = rtype_clone(&entry.fields[j].ty);
                found = true;
                break;
            }
            field_offset += s;
            j += 1;
        }
        if !found {
            return None;
        }
        i += 1;
    }
    let mut vts: Vec<wasm::ValType> = Vec::new();
    flatten_rtype(&current_ty, ctx.structs, &mut vts);
    Some((ctx.locals[bidx].wasm_start + offset, vts.len() as u32))
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
        ExprKind::Borrow { inner, mutable } => {
            let inner_ty = codegen_expr(ctx, inner)?;
            Ok(RType::Ref {
                inner: Box::new(inner_ty),
                mutable: *mutable,
            })
        }
        ExprKind::Block(block) => codegen_block_expr(ctx, block.as_ref()),
    }
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
            // Layout: [low, high]. Literal value fits in u64, high half is 0.
            ctx.instructions
                .push(wasm::Instruction::I64Const(value as i64));
            ctx.instructions.push(wasm::Instruction::I64Const(0));
        }
        _ => {
            // u8/i8/u16/i16/u32/i32/usize/isize all live in a single i32 slot.
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
    let mut i = 0;
    while i < ctx.locals.len() {
        if ctx.locals[i].name == *name {
            let start = ctx.locals[i].wasm_start;
            let size = ctx.locals[i].size;
            let rt = rtype_clone(&ctx.locals[i].rtype);
            let mut k = 0;
            while k < size {
                ctx.instructions
                    .push(wasm::Instruction::LocalGet(start + k));
                k += 1;
            }
            return Ok(rt);
        }
        i += 1;
    }
    unreachable!("typeck verified the variable exists");
}

fn codegen_call(ctx: &mut FnCtx, call: &Call) -> Result<RType, Error> {
    let (func_idx, return_rt, param_is_mut_ref) = {
        let mut full = clone_path(&ctx.current_module);
        let mut i = 0;
        while i < call.callee.segments.len() {
            full.push(call.callee.segments[i].name.clone());
            i += 1;
        }
        let entry = func_lookup(ctx.funcs, &full).expect("typeck resolved this call");
        let rt = match &entry.return_type {
            Some(rt) => rtype_clone(rt),
            None => unreachable!("typeck rejects unit functions used as values"),
        };
        let mut flags: Vec<bool> = Vec::new();
        let mut k = 0;
        while k < entry.param_types.len() {
            flags.push(is_ref_mutable(&entry.param_types[k]));
            k += 1;
        }
        (entry.idx, rt, flags)
    };
    // Compute write-back ranges for each `&mut` arg before emitting code, so
    // the source's locals are fully valid at call time.
    let mut writebacks: Vec<(u32, u32)> = Vec::new();
    let mut i = 0;
    while i < call.args.len() {
        if param_is_mut_ref[i] {
            let r = mut_ref_source_range(ctx, &call.args[i])
                .expect("typeck verified the &mut arg is a place expression");
            writebacks.push(r);
        }
        i += 1;
    }
    // Push each arg's flat values onto the stack.
    let mut i = 0;
    while i < call.args.len() {
        codegen_expr(ctx, &call.args[i])?;
        i += 1;
    }
    ctx.instructions.push(wasm::Instruction::Call(func_idx));
    // After the call, the stack has [result_flat..., wb_1_flat, wb_2_flat, ...].
    // Pop each write-back range in reverse order back into source locals.
    let mut i = writebacks.len();
    while i > 0 {
        i -= 1;
        let (start, size) = writebacks[i];
        let mut k = 0;
        while k < size {
            ctx.instructions
                .push(wasm::Instruction::LocalSet(start + size - 1 - k));
            k += 1;
        }
    }
    Ok(return_rt)
}

fn codegen_struct_lit(ctx: &mut FnCtx, lit: &StructLit) -> Result<RType, Error> {
    let mut full = clone_path(&ctx.current_module);
    let mut i = 0;
    while i < lit.path.segments.len() {
        full.push(lit.path.segments[i].name.clone());
        i += 1;
    }

    // Field layouts in declaration order: (name, offset_in_wasm_scalars, valtypes).
    struct FieldLayout {
        name: String,
        offset: u32,
        valtypes: Vec<wasm::ValType>,
    }
    let layouts: Vec<FieldLayout> = {
        let entry = struct_lookup(ctx.structs, &full).expect("typeck resolved this struct");
        let mut out: Vec<FieldLayout> = Vec::new();
        let mut offset: u32 = 0;
        let mut i = 0;
        while i < entry.fields.len() {
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&entry.fields[i].ty, ctx.structs, &mut vts);
            let size = vts.len() as u32;
            out.push(FieldLayout {
                name: entry.fields[i].name.clone(),
                offset,
                valtypes: vts,
            });
            offset += size;
            i += 1;
        }
        out
    };
    let total_size: u32 = layouts.iter().map(|l| l.valtypes.len() as u32).sum();

    // Allocate temporary locals for the entire flat struct, sized by each field.
    let temp_start = ctx.next_local;
    let mut k = 0;
    while k < layouts.len() {
        let mut j = 0;
        while j < layouts[k].valtypes.len() {
            ctx.extra_locals.push(layouts[k].valtypes[j].copy());
            ctx.next_local += 1;
            j += 1;
        }
        k += 1;
    }

    // Walk fields in source order; codegen each value, then LocalSet into its
    // declaration-order slot. This way the source-order walk lines up with
    // typeck's lit_idx / let_idx counters but the struct is still laid out in
    // declaration order on the stack.
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
                        temp_start + layouts[layout_idx].offset + size - 1 - k,
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

    Ok(RType::Struct(full))
}

fn codegen_field_access(ctx: &mut FnCtx, fa: &FieldAccess) -> Result<RType, Error> {
    let base_type = codegen_expr(ctx, &fa.base)?;
    let total = rtype_size(&base_type, ctx.structs);

    let struct_path = match &base_type {
        RType::Struct(p) => clone_path(p),
        RType::Ref { inner, .. } => match inner.as_ref() {
            RType::Struct(p) => clone_path(p),
            _ => unreachable!("typeck rejects field access on non-struct"),
        },
        RType::Int(_) => unreachable!("typeck rejects field access on non-struct"),
    };

    let (offset, field_valtypes, field_ty) = {
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let mut offset: u32 = 0;
        let mut found: Option<(u32, Vec<wasm::ValType>, RType)> = None;
        let mut i = 0;
        while i < entry.fields.len() {
            let f = &entry.fields[i];
            let mut vts: Vec<wasm::ValType> = Vec::new();
            flatten_rtype(&f.ty, ctx.structs, &mut vts);
            let s = vts.len() as u32;
            if f.name == fa.field {
                found = Some((offset, vts, rtype_clone(&f.ty)));
                break;
            }
            offset += s;
            i += 1;
        }
        found.expect("typeck verified field")
    };
    let size = field_valtypes.len() as u32;

    let drop_top = total - offset - size;
    let mut i = 0;
    while i < drop_top {
        ctx.instructions.push(wasm::Instruction::Drop);
        i += 1;
    }

    let stash_start = ctx.next_local;
    let mut k = 0;
    while k < field_valtypes.len() {
        ctx.extra_locals.push(field_valtypes[k].copy());
        ctx.next_local += 1;
        k += 1;
    }
    let mut k = 0;
    while k < size {
        ctx.instructions
            .push(wasm::Instruction::LocalSet(stash_start + size - 1 - k));
        k += 1;
    }

    let mut i = 0;
    while i < offset {
        ctx.instructions.push(wasm::Instruction::Drop);
        i += 1;
    }

    let mut k = 0;
    while k < size {
        ctx.instructions
            .push(wasm::Instruction::LocalGet(stash_start + k));
        k += 1;
    }

    Ok(field_ty)
}
