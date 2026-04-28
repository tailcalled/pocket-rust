use crate::ast::{
    Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, Stmt, StructLit,
};
use crate::span::Error;
use crate::typeck::{
    FuncTable, RType, StructTable, clone_path, flatten_rtype, func_lookup, rtype_clone,
    rtype_size, struct_lookup,
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
    };
    codegen_block(&mut ctx, &func.body)?;

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
        }
        i += 1;
    }
    if let Some(expr) = &block.tail {
        codegen_expr(ctx, expr)?;
    }
    Ok(())
}

fn codegen_let_stmt(ctx: &mut FnCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    let value_ty = codegen_expr(ctx, &let_stmt.value)?;
    let size = rtype_size(&value_ty, ctx.structs);
    let start = ctx.next_local;
    let mut k = 0;
    while k < size {
        ctx.extra_locals.push(wasm::ValType::I32);
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

fn codegen_expr(ctx: &mut FnCtx, expr: &Expr) -> Result<RType, Error> {
    match &expr.kind {
        ExprKind::UsizeLit(n) => {
            let bits = *n as u32;
            ctx.instructions
                .push(wasm::Instruction::I32Const(bits as i32));
            Ok(RType::Usize)
        }
        ExprKind::Var(name) => codegen_var(ctx, name),
        ExprKind::Call(call) => codegen_call(ctx, call),
        ExprKind::StructLit(lit) => codegen_struct_lit(ctx, lit),
        ExprKind::FieldAccess(fa) => codegen_field_access(ctx, fa),
        ExprKind::Borrow(inner) => {
            let inner_ty = codegen_expr(ctx, inner)?;
            Ok(RType::Ref(Box::new(inner_ty)))
        }
        ExprKind::Block(block) => codegen_block_expr(ctx, block.as_ref()),
    }
}

fn codegen_block_expr(ctx: &mut FnCtx, block: &Block) -> Result<RType, Error> {
    let mark = ctx.locals.len();
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => codegen_let_stmt(ctx, let_stmt)?,
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
    let (func_idx, return_rt) = {
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
        (entry.idx, rt)
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
    let mut full = clone_path(&ctx.current_module);
    let mut i = 0;
    while i < lit.path.segments.len() {
        full.push(lit.path.segments[i].name.clone());
        i += 1;
    }

    let def_field_names: Vec<String> = {
        let entry = struct_lookup(ctx.structs, &full).expect("typeck resolved this struct");
        let mut names: Vec<String> = Vec::new();
        let mut i = 0;
        while i < entry.fields.len() {
            names.push(entry.fields[i].name.clone());
            i += 1;
        }
        names
    };

    let mut i = 0;
    while i < def_field_names.len() {
        let mut k = 0;
        while k < lit.fields.len() {
            if lit.fields[k].name == def_field_names[i] {
                codegen_expr(ctx, &lit.fields[k].value)?;
                break;
            }
            k += 1;
        }
        i += 1;
    }

    Ok(RType::Struct(full))
}

fn codegen_field_access(ctx: &mut FnCtx, fa: &FieldAccess) -> Result<RType, Error> {
    let base_type = codegen_expr(ctx, &fa.base)?;
    let total = rtype_size(&base_type, ctx.structs);

    let struct_path = match &base_type {
        RType::Struct(p) => clone_path(p),
        RType::Ref(inner) => match inner.as_ref() {
            RType::Struct(p) => clone_path(p),
            _ => unreachable!("typeck rejects field access on non-struct"),
        },
        RType::Usize => unreachable!("typeck rejects field access on non-struct"),
    };

    let (offset, size, field_ty) = {
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let mut offset: u32 = 0;
        let mut found: Option<(u32, u32, RType)> = None;
        let mut i = 0;
        while i < entry.fields.len() {
            let f = &entry.fields[i];
            let s = rtype_size(&f.ty, ctx.structs);
            if f.name == fa.field {
                found = Some((offset, s, rtype_clone(&f.ty)));
                break;
            }
            offset += s;
            i += 1;
        }
        found.expect("typeck verified field")
    };

    let drop_top = total - offset - size;
    let mut i = 0;
    while i < drop_top {
        ctx.instructions.push(wasm::Instruction::Drop);
        i += 1;
    }

    let stash_start = ctx.next_local;
    let mut k = 0;
    while k < size {
        ctx.extra_locals.push(wasm::ValType::I32);
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
