// Safety check pass.
//
// Single rule: dereferencing a raw pointer (`*const T` / `*mut T`) must
// happen inside an `unsafe { ... }` block. Dereferencing a reference
// (`&T` / `&mut T`) is always safe.
//
// We don't redo type analysis here — typeck records, per `Deref` expression
// in source-DFS order, whether the operand resolved to a raw pointer
// (`FnSymbol.deref_is_raw`). Safeck walks the AST in lockstep with that
// vector, tracking an `in_unsafe` boolean across `unsafe { ... }` boundaries.

use crate::ast::{Block, Call, Expr, ExprKind, Function, Item, Module, Stmt, StructLit};
use crate::span::Error;
use crate::typeck::{FuncTable, clone_path, func_lookup};

pub fn check(root: &Module, funcs: &FuncTable) -> Result<(), Error> {
    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    let mut current_file = root.source_file.clone();
    check_module(root, &mut path, &mut current_file, funcs)?;
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn check_module(
    module: &Module,
    path: &mut Vec<String>,
    current_file: &mut String,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => check_function(f, path, current_file, funcs)?,
            Item::Module(m) => {
                path.push(m.name.clone());
                check_module(m, path, current_file, funcs)?;
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
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut full = clone_path(current_module);
    full.push(func.name.clone());
    let entry = func_lookup(funcs, &full).expect("typeck registered this function");
    let mut state = SafeState {
        deref_is_raw: &entry.deref_is_raw,
        deref_idx: 0,
        in_unsafe: false,
        file: current_file.to_string(),
    };
    walk_block(&mut state, &func.body)?;
    Ok(())
}

struct SafeState<'a> {
    deref_is_raw: &'a Vec<bool>,
    deref_idx: usize,
    in_unsafe: bool,
    file: String,
}

fn walk_block(state: &mut SafeState, block: &Block) -> Result<(), Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => walk_expr(state, &let_stmt.value)?,
            Stmt::Assign(assign) => {
                walk_expr(state, &assign.lhs)?;
                walk_expr(state, &assign.rhs)?;
            }
            Stmt::Expr(expr) => walk_expr(state, expr)?,
        }
        i += 1;
    }
    if let Some(tail) = &block.tail {
        walk_expr(state, tail)?;
    }
    Ok(())
}

fn walk_expr(state: &mut SafeState, expr: &Expr) -> Result<(), Error> {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::Var(_) => Ok(()),
        ExprKind::Borrow { inner, .. } => walk_expr(state, inner),
        ExprKind::Cast { inner, .. } => walk_expr(state, inner),
        ExprKind::Deref(inner) => {
            walk_expr(state, inner)?;
            let is_raw = state.deref_is_raw[state.deref_idx];
            state.deref_idx += 1;
            if is_raw && !state.in_unsafe {
                return Err(Error {
                    file: state.file.clone(),
                    message:
                        "dereference of raw pointer is unsafe and requires an `unsafe` block"
                            .to_string(),
                    span: expr.span.copy(),
                });
            }
            Ok(())
        }
        ExprKind::Call(call) => walk_call(state, call),
        ExprKind::StructLit(lit) => walk_struct_lit(state, lit),
        ExprKind::FieldAccess(fa) => walk_expr(state, &fa.base),
        ExprKind::Block(block) => walk_block(state, block.as_ref()),
        ExprKind::Unsafe(block) => {
            let saved = state.in_unsafe;
            state.in_unsafe = true;
            walk_block(state, block.as_ref())?;
            state.in_unsafe = saved;
            Ok(())
        }
    }
}

fn walk_call(state: &mut SafeState, call: &Call) -> Result<(), Error> {
    let mut i = 0;
    while i < call.args.len() {
        walk_expr(state, &call.args[i])?;
        i += 1;
    }
    Ok(())
}

fn walk_struct_lit(state: &mut SafeState, lit: &StructLit) -> Result<(), Error> {
    let mut i = 0;
    while i < lit.fields.len() {
        walk_expr(state, &lit.fields[i].value)?;
        i += 1;
    }
    Ok(())
}
