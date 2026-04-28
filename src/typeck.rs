use crate::ast::{
    Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, PathSegment, Stmt,
    StructLit, Type, TypeKind,
};
use crate::span::{Error, Span};

pub enum RType {
    Usize,
    Struct(Vec<String>),
    Ref(Box<RType>),
}

pub fn rtype_clone(t: &RType) -> RType {
    match t {
        RType::Usize => RType::Usize,
        RType::Struct(p) => RType::Struct(clone_path(p)),
        RType::Ref(inner) => RType::Ref(Box::new(rtype_clone(inner))),
    }
}

pub fn rtype_eq(a: &RType, b: &RType) -> bool {
    match (a, b) {
        (RType::Usize, RType::Usize) => true,
        (RType::Struct(pa), RType::Struct(pb)) => path_eq(pa, pb),
        (RType::Ref(ia), RType::Ref(ib)) => rtype_eq(ia, ib),
        _ => false,
    }
}

pub fn rtype_to_string(t: &RType) -> String {
    match t {
        RType::Usize => "usize".to_string(),
        RType::Struct(p) => place_to_string(p),
        RType::Ref(inner) => format!("&{}", rtype_to_string(inner)),
    }
}

pub fn rtype_size(ty: &RType, structs: &StructTable) -> u32 {
    match ty {
        RType::Usize => 1,
        RType::Struct(p) => {
            let entry = struct_lookup(structs, p).expect("resolved struct");
            let mut s: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                s += rtype_size(&entry.fields[i].ty, structs);
                i += 1;
            }
            s
        }
        RType::Ref(inner) => rtype_size(inner, structs),
    }
}

pub fn flatten_rtype(ty: &RType, structs: &StructTable, out: &mut Vec<crate::wasm::ValType>) {
    match ty {
        RType::Usize => out.push(crate::wasm::ValType::I32),
        RType::Struct(p) => {
            let entry = struct_lookup(structs, p).expect("resolved struct");
            let mut i = 0;
            while i < entry.fields.len() {
                flatten_rtype(&entry.fields[i].ty, structs, out);
                i += 1;
            }
        }
        RType::Ref(inner) => flatten_rtype(inner, structs, out),
    }
}

pub fn is_copy(t: &RType) -> bool {
    match t {
        RType::Usize => true,
        RType::Struct(_) => false,
        RType::Ref(_) => true,
    }
}

pub struct RTypedField {
    pub name: String,
    pub name_span: Span,
    pub ty: RType,
}

pub struct StructEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub fields: Vec<RTypedField>,
}

pub struct StructTable {
    pub entries: Vec<StructEntry>,
}

pub fn struct_lookup<'a>(table: &'a StructTable, path: &Vec<String>) -> Option<&'a StructEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

pub struct FnSymbol {
    pub path: Vec<String>,
    pub idx: u32,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub let_types: Vec<RType>,
}

pub struct FuncTable {
    pub entries: Vec<FnSymbol>,
}

pub fn func_lookup<'a>(table: &'a FuncTable, path: &Vec<String>) -> Option<&'a FnSymbol> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

pub fn clone_path(path: &Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < path.len() {
        out.push(path[i].clone());
        i += 1;
    }
    out
}

pub fn path_eq(a: &Vec<String>, b: &Vec<String>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub fn segments_to_string(segs: &Vec<PathSegment>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < segs.len() {
        if i > 0 {
            s.push_str("::");
        }
        s.push_str(&segs[i].name);
        i += 1;
    }
    s
}

pub fn place_to_string(p: &Vec<String>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < p.len() {
        if i > 0 {
            s.push('.');
        }
        s.push_str(&p[i]);
        i += 1;
    }
    s
}

pub fn resolve_type(
    ty: &Type,
    current_module: &Vec<String>,
    structs: &StructTable,
    file: &str,
) -> Result<RType, Error> {
    match &ty.kind {
        TypeKind::Usize => Ok(RType::Usize),
        TypeKind::Struct(path) => {
            let mut full = clone_path(current_module);
            let mut i = 0;
            while i < path.segments.len() {
                full.push(path.segments[i].name.clone());
                i += 1;
            }
            if struct_lookup(structs, &full).is_some() {
                Ok(RType::Struct(full))
            } else {
                Err(Error {
                    file: file.to_string(),
                    message: format!("unknown type: {}", segments_to_string(&path.segments)),
                    span: path.span.copy(),
                })
            }
        }
        TypeKind::Ref(inner) => {
            let r = resolve_type(inner, current_module, structs, file)?;
            Ok(RType::Ref(Box::new(r)))
        }
    }
}

// ----- Top-level entry point -----

pub fn check(
    root: &Module,
    structs: &mut StructTable,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_struct_names(root, &mut path, structs);

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_struct_fields(root, &mut path, structs)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_funcs(root, &mut path, funcs, next_idx, structs)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    let mut current_file = root.source_file.clone();
    check_module(root, &mut path, &mut current_file, structs, funcs)?;

    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn collect_struct_names(module: &Module, path: &mut Vec<String>, table: &mut StructTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = clone_path(path);
                full.push(sd.name.clone());
                table.entries.push(StructEntry {
                    path: full,
                    name_span: sd.name_span.copy(),
                    file: module.source_file.clone(),
                    fields: Vec::new(),
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_struct_names(m, path, table);
                path.pop();
            }
            Item::Function(_) => {}
        }
        i += 1;
    }
}

fn resolve_struct_fields(
    module: &Module,
    path: &mut Vec<String>,
    table: &mut StructTable,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = clone_path(path);
                full.push(sd.name.clone());
                let mut resolved: Vec<RTypedField> = Vec::new();
                let mut k = 0;
                while k < sd.fields.len() {
                    let rt =
                        resolve_type(&sd.fields[k].ty, path, table, &module.source_file)?;
                    if let RType::Ref(_) = &rt {
                        return Err(Error {
                            file: module.source_file.clone(),
                            message: "struct fields cannot have reference types".to_string(),
                            span: sd.fields[k].ty.span.copy(),
                        });
                    }
                    resolved.push(RTypedField {
                        name: sd.fields[k].name.clone(),
                        name_span: sd.fields[k].name_span.copy(),
                        ty: rt,
                    });
                    k += 1;
                }
                let mut e = 0;
                while e < table.entries.len() {
                    if path_eq(&table.entries[e].path, &full) {
                        table.entries[e].fields = resolved;
                        break;
                    }
                    e += 1;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_struct_fields(m, path, table)?;
                path.pop();
            }
            Item::Function(_) => {}
        }
        i += 1;
    }
    Ok(())
}

fn collect_funcs(
    module: &Module,
    path: &mut Vec<String>,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
    structs: &StructTable,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                let mut full = clone_path(path);
                full.push(f.name.clone());
                let mut param_types: Vec<RType> = Vec::new();
                let mut k = 0;
                while k < f.params.len() {
                    let rt = resolve_type(
                        &f.params[k].ty,
                        path,
                        structs,
                        &module.source_file,
                    )?;
                    param_types.push(rt);
                    k += 1;
                }
                let return_type = match &f.return_type {
                    Some(ty) => {
                        let rt = resolve_type(ty, path, structs, &module.source_file)?;
                        if let RType::Ref(_) = &rt {
                            return Err(Error {
                                file: module.source_file.clone(),
                                message: "functions cannot return reference types".to_string(),
                                span: ty.span.copy(),
                            });
                        }
                        Some(rt)
                    }
                    None => None,
                };
                funcs.entries.push(FnSymbol {
                    path: full,
                    idx: *next_idx,
                    param_types,
                    return_type,
                    let_types: Vec::new(),
                });
                *next_idx += 1;
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_funcs(m, path, funcs, next_idx, structs)?;
                path.pop();
            }
            Item::Struct(_) => {}
        }
        i += 1;
    }
    Ok(())
}

// ----- Body check pass -----

struct CheckCtx<'a> {
    locals: Vec<(String, RType)>,
    let_types: Vec<RType>,
    current_module: &'a Vec<String>,
    current_file: &'a str,
    structs: &'a StructTable,
    funcs: &'a FuncTable,
}

fn check_module(
    module: &Module,
    path: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => check_function(f, path, current_file, structs, funcs)?,
            Item::Module(m) => {
                path.push(m.name.clone());
                check_module(m, path, current_file, structs, funcs)?;
                path.pop();
            }
            Item::Struct(_) => {}
        }
        i += 1;
    }
    *current_file = saved;
    Ok(())
}

fn check_function(
    func: &Function,
    current_module: &Vec<String>,
    current_file: &str,
    structs: &StructTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let mut locals: Vec<(String, RType)> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let rt = resolve_type(&func.params[k].ty, current_module, structs, current_file)?;
        locals.push((func.params[k].name.clone(), rt));
        k += 1;
    }
    let return_rt: Option<RType> = match &func.return_type {
        Some(ty) => Some(resolve_type(ty, current_module, structs, current_file)?),
        None => None,
    };

    let let_types = {
        let mut ctx = CheckCtx {
            locals,
            let_types: Vec::new(),
            current_module,
            current_file,
            structs,
            funcs: &*funcs,
        };
        check_block(&mut ctx, &func.body, &return_rt)?;
        ctx.let_types
    };

    let mut full = clone_path(current_module);
    full.push(func.name.clone());
    let mut e = 0;
    while e < funcs.entries.len() {
        if path_eq(&funcs.entries[e].path, &full) {
            funcs.entries[e].let_types = let_types;
            break;
        }
        e += 1;
    }
    Ok(())
}

fn check_block(
    ctx: &mut CheckCtx,
    block: &Block,
    return_type: &Option<RType>,
) -> Result<(), Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => check_let_stmt(ctx, let_stmt)?,
        }
        i += 1;
    }
    match (&block.tail, return_type) {
        (Some(expr), Some(expected)) => {
            let actual = check_expr(ctx, expr)?;
            if !rtype_eq(&actual, expected) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "expected return type `{}`, got `{}`",
                        rtype_to_string(expected),
                        rtype_to_string(&actual)
                    ),
                    span: expr.span.copy(),
                });
            }
            Ok(())
        }
        (None, None) => Ok(()),
        (Some(expr), None) => Err(Error {
            file: ctx.current_file.to_string(),
            message: "function returns unit but body has a tail expression".to_string(),
            span: expr.span.copy(),
        }),
        (None, Some(_)) => Err(Error {
            file: ctx.current_file.to_string(),
            message: "function expects a return value but body is empty".to_string(),
            span: block.span.copy(),
        }),
    }
}

fn check_let_stmt(ctx: &mut CheckCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    let value_ty = check_expr(ctx, &let_stmt.value)?;
    let final_ty = match &let_stmt.ty {
        Some(annotation) => {
            let annot_ty = resolve_type(
                annotation,
                ctx.current_module,
                ctx.structs,
                ctx.current_file,
            )?;
            if !rtype_eq(&value_ty, &annot_ty) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "let initializer has type `{}`, expected `{}`",
                        rtype_to_string(&value_ty),
                        rtype_to_string(&annot_ty)
                    ),
                    span: let_stmt.value.span.copy(),
                });
            }
            annot_ty
        }
        None => value_ty,
    };
    ctx.locals
        .push((let_stmt.name.clone(), rtype_clone(&final_ty)));
    ctx.let_types.push(final_ty);
    Ok(())
}

fn check_expr(ctx: &mut CheckCtx, expr: &Expr) -> Result<RType, Error> {
    match &expr.kind {
        ExprKind::UsizeLit(n) => {
            if *n > (u32::MAX as u64) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("usize literal out of range: {}", n),
                    span: expr.span.copy(),
                });
            }
            Ok(RType::Usize)
        }
        ExprKind::Var(name) => {
            let mut i = 0;
            while i < ctx.locals.len() {
                if ctx.locals[i].0 == *name {
                    return Ok(rtype_clone(&ctx.locals[i].1));
                }
                i += 1;
            }
            Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown variable: `{}`", name),
                span: expr.span.copy(),
            })
        }
        ExprKind::Call(call) => check_call(ctx, call, expr),
        ExprKind::StructLit(lit) => check_struct_lit(ctx, lit, expr),
        ExprKind::FieldAccess(fa) => check_field_access(ctx, fa, expr),
        ExprKind::Borrow(inner) => {
            let inner_ty = check_expr(ctx, inner)?;
            Ok(RType::Ref(Box::new(inner_ty)))
        }
    }
}

fn check_call(ctx: &mut CheckCtx, call: &Call, call_expr: &Expr) -> Result<RType, Error> {
    let mut full = clone_path(ctx.current_module);
    let mut i = 0;
    while i < call.callee.segments.len() {
        full.push(call.callee.segments[i].name.clone());
        i += 1;
    }
    let entry = match func_lookup(ctx.funcs, &full) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unresolved function: {}",
                    segments_to_string(&call.callee.segments)
                ),
                span: call.callee.span.copy(),
            });
        }
    };
    if call.args.len() != entry.param_types.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected {}, got {}",
                segments_to_string(&call.callee.segments),
                entry.param_types.len(),
                call.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let mut param_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < entry.param_types.len() {
        param_types.push(rtype_clone(&entry.param_types[k]));
        k += 1;
    }
    let return_rt = match &entry.return_type {
        Some(rt) => Some(rtype_clone(rt)),
        None => None,
    };

    let mut i = 0;
    while i < call.args.len() {
        let arg_ty = check_expr(ctx, &call.args[i])?;
        if !rtype_eq(&arg_ty, &param_types[i]) {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "argument {} to `{}` has type `{}`, expected `{}`",
                    i + 1,
                    segments_to_string(&call.callee.segments),
                    rtype_to_string(&arg_ty),
                    rtype_to_string(&param_types[i])
                ),
                span: call.args[i].span.copy(),
            });
        }
        i += 1;
    }

    match return_rt {
        Some(rt) => Ok(rt),
        None => Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "function `{}` returns unit and can't be used as a value",
                segments_to_string(&call.callee.segments)
            ),
            span: call_expr.span.copy(),
        }),
    }
}

fn check_struct_lit(
    ctx: &mut CheckCtx,
    lit: &StructLit,
    lit_expr: &Expr,
) -> Result<RType, Error> {
    let mut full = clone_path(ctx.current_module);
    let mut i = 0;
    while i < lit.path.segments.len() {
        full.push(lit.path.segments[i].name.clone());
        i += 1;
    }

    let entry = match struct_lookup(ctx.structs, &full) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unknown struct: {}",
                    segments_to_string(&lit.path.segments)
                ),
                span: lit.path.span.copy(),
            });
        }
    };
    let mut def_field_names: Vec<String> = Vec::new();
    let mut def_field_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < entry.fields.len() {
        def_field_names.push(entry.fields[k].name.clone());
        def_field_types.push(rtype_clone(&entry.fields[k].ty));
        k += 1;
    }

    // No unknown fields and no duplicates.
    let mut i = 0;
    while i < lit.fields.len() {
        let mut found = false;
        let mut j = 0;
        while j < def_field_names.len() {
            if lit.fields[i].name == def_field_names[j] {
                found = true;
                break;
            }
            j += 1;
        }
        if !found {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "struct `{}` has no field `{}`",
                    segments_to_string(&lit.path.segments),
                    lit.fields[i].name
                ),
                span: lit.fields[i].name_span.copy(),
            });
        }
        let mut k = i + 1;
        while k < lit.fields.len() {
            if lit.fields[k].name == lit.fields[i].name {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("field `{}` is initialized twice", lit.fields[i].name),
                    span: lit.fields[k].name_span.copy(),
                });
            }
            k += 1;
        }
        i += 1;
    }
    // Every def field initialized.
    let mut i = 0;
    while i < def_field_names.len() {
        let mut present = false;
        let mut j = 0;
        while j < lit.fields.len() {
            if lit.fields[j].name == def_field_names[i] {
                present = true;
                break;
            }
            j += 1;
        }
        if !present {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("missing field `{}`", def_field_names[i]),
                span: lit_expr.span.copy(),
            });
        }
        i += 1;
    }

    // Each init's type matches the declared field type.
    let mut i = 0;
    while i < def_field_names.len() {
        let mut k = 0;
        while k < lit.fields.len() {
            if lit.fields[k].name == def_field_names[i] {
                let init_ty = check_expr(ctx, &lit.fields[k].value)?;
                if !rtype_eq(&init_ty, &def_field_types[i]) {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: format!(
                            "field `{}` has type `{}`, expected `{}`",
                            def_field_names[i],
                            rtype_to_string(&init_ty),
                            rtype_to_string(&def_field_types[i])
                        ),
                        span: lit.fields[k].value.span.copy(),
                    });
                }
                break;
            }
            k += 1;
        }
        i += 1;
    }

    Ok(RType::Struct(full))
}

fn check_field_access(
    ctx: &mut CheckCtx,
    fa: &FieldAccess,
    _fa_expr: &Expr,
) -> Result<RType, Error> {
    let base_type = check_expr(ctx, &fa.base)?;
    let (struct_path, through_ref) = match &base_type {
        RType::Struct(p) => (clone_path(p), false),
        RType::Ref(inner) => match inner.as_ref() {
            RType::Struct(p) => (clone_path(p), true),
            _ => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "field access on non-struct value".to_string(),
                    span: fa.base.span.copy(),
                });
            }
        },
        RType::Usize => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "field access on non-struct value".to_string(),
                span: fa.base.span.copy(),
            });
        }
    };
    let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
    let mut i = 0;
    while i < entry.fields.len() {
        if entry.fields[i].name == fa.field {
            let field_ty = rtype_clone(&entry.fields[i].ty);
            if through_ref && !is_copy(&field_ty) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of borrow: field `{}` of `{}` has non-Copy type `{}`",
                        fa.field,
                        place_to_string(&struct_path),
                        rtype_to_string(&field_ty)
                    ),
                    span: fa.field_span.copy(),
                });
            }
            return Ok(field_ty);
        }
        i += 1;
    }
    Err(Error {
        file: ctx.current_file.to_string(),
        message: format!(
            "no field `{}` on `{}`",
            fa.field,
            place_to_string(&struct_path)
        ),
        span: fa.field_span.copy(),
    })
}
