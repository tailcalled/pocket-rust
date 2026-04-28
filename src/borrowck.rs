use crate::ast::{Block, Expr, ExprKind, Function, Item, Module, Stmt};
use crate::span::{Error, Span};
use crate::typeck::{FuncTable, RType, StructTable, clone_path, func_lookup, rtype_clone};

pub fn check(
    root: &Module,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut current_file = root.source_file.clone();
    let mut current_module: Vec<String> = Vec::new();
    push_root_name(&mut current_module, root);
    check_module(root, &mut current_module, &mut current_file, structs, funcs)?;
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn check_module(
    module: &Module,
    current_module: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                check_function(f, current_module, current_file, structs, funcs)?
            }
            Item::Module(m) => {
                current_module.push(m.name.clone());
                check_module(m, current_module, current_file, structs, funcs)?;
                current_module.pop();
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
    _structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut full = clone_path(current_module);
    full.push(func.name.clone());
    let entry = func_lookup(funcs, &full).expect("typeck registered this function");

    let mut locals: Vec<(String, RType)> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        locals.push((
            func.params[k].name.clone(),
            rtype_clone(&entry.param_types[k]),
        ));
        k += 1;
    }

    let mut ctx = BorrowCtx {
        moved: Vec::new(),
        borrows: Vec::new(),
        locals,
        file: current_file.to_string(),
    };
    track_block(&mut ctx, &func.body, &entry.let_types)?;
    Ok(())
}

struct BorrowCtx {
    moved: Vec<Vec<String>>,
    borrows: Vec<Vec<String>>,
    locals: Vec<(String, RType)>,
    file: String,
}

fn track_block(
    ctx: &mut BorrowCtx,
    block: &Block,
    let_types: &Vec<RType>,
) -> Result<(), Error> {
    let mut let_idx: usize = 0;
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                track_expr(ctx, &let_stmt.value)?;
                ctx.locals
                    .push((let_stmt.name.clone(), rtype_clone(&let_types[let_idx])));
                let_idx += 1;
            }
        }
        i += 1;
    }
    if let Some(tail) = &block.tail {
        track_expr(ctx, tail)?;
    }
    Ok(())
}

fn is_ref_local(locals: &Vec<(String, RType)>, name: &str) -> bool {
    let mut i = 0;
    while i < locals.len() {
        if locals[i].0 == *name {
            return matches!(locals[i].1, RType::Ref(_));
        }
        i += 1;
    }
    false
}

fn track_expr(ctx: &mut BorrowCtx, expr: &Expr) -> Result<(), Error> {
    match &expr.kind {
        ExprKind::UsizeLit(_) => Ok(()),
        ExprKind::Var(name) => {
            if is_ref_local(&ctx.locals, name) {
                Ok(())
            } else {
                let mut place: Vec<String> = Vec::new();
                place.push(name.clone());
                try_move(ctx, place, expr.span.copy())
            }
        }
        ExprKind::FieldAccess(fa) => match extract_place(expr) {
            Some(place) => {
                if is_ref_local(&ctx.locals, &place[0]) {
                    Ok(())
                } else {
                    try_move(ctx, place, expr.span.copy())
                }
            }
            None => track_expr(ctx, &fa.base),
        },
        ExprKind::Call(call) => {
            // Borrows created while evaluating args die when the call returns.
            // Moves are permanent.
            let borrow_mark = ctx.borrows.len();
            let mut i = 0;
            while i < call.args.len() {
                track_expr(ctx, &call.args[i])?;
                i += 1;
            }
            ctx.borrows.truncate(borrow_mark);
            Ok(())
        }
        ExprKind::StructLit(lit) => {
            let mut i = 0;
            while i < lit.fields.len() {
                track_expr(ctx, &lit.fields[i].value)?;
                i += 1;
            }
            Ok(())
        }
        ExprKind::Borrow(inner) => match extract_place(inner) {
            Some(place) => try_borrow(ctx, place, expr.span.copy()),
            None => track_expr(ctx, inner),
        },
    }
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

fn try_move(ctx: &mut BorrowCtx, place: Vec<String>, span: Span) -> Result<(), Error> {
    let mut i = 0;
    while i < ctx.moved.len() {
        if paths_share_prefix(&ctx.moved[i], &place) {
            return Err(Error {
                file: ctx.file.clone(),
                message: format!("`{}` was already moved", place_to_string(&place)),
                span,
            });
        }
        i += 1;
    }
    let mut i = 0;
    while i < ctx.borrows.len() {
        if paths_share_prefix(&ctx.borrows[i], &place) {
            return Err(Error {
                file: ctx.file.clone(),
                message: format!(
                    "cannot move `{}` while it is borrowed",
                    place_to_string(&place)
                ),
                span,
            });
        }
        i += 1;
    }
    ctx.moved.push(place);
    Ok(())
}

fn try_borrow(ctx: &mut BorrowCtx, place: Vec<String>, span: Span) -> Result<(), Error> {
    let mut i = 0;
    while i < ctx.moved.len() {
        if paths_share_prefix(&ctx.moved[i], &place) {
            return Err(Error {
                file: ctx.file.clone(),
                message: format!(
                    "cannot borrow `{}`: it has been moved",
                    place_to_string(&place)
                ),
                span,
            });
        }
        i += 1;
    }
    ctx.borrows.push(place);
    Ok(())
}

fn paths_share_prefix(a: &Vec<String>, b: &Vec<String>) -> bool {
    let m = if a.len() < b.len() { a.len() } else { b.len() };
    let mut i = 0;
    while i < m {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn place_to_string(p: &Vec<String>) -> String {
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
