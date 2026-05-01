// Safety check pass.
//
// Single rule: dereferencing a raw pointer (`*const T` / `*mut T`) must
// happen inside an `unsafe { ... }` block. Dereferencing a reference
// (`&T` / `&mut T`) is always safe.
//
// We don't redo type analysis here — typeck records each Expr's resolved
// type at `FnSymbol.expr_types[expr.id]`. For a `Deref(inner)`, safeck reads
// the inner expr's type at its NodeId and flags raw-pointer derefs outside
// `unsafe { ... }` blocks.

use crate::ast::{Block, Call, Expr, ExprKind, Function, Item, MethodCall, Module, Stmt, StructLit};
use crate::span::Error;
use crate::typeck::{FuncTable, func_lookup, template_lookup};

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
            Item::Enum(_) => {}
            Item::Impl(ib) => {
                if ib.trait_path.is_some() {
                    i += 1;
                    continue;
                }
                let target_name = match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
                        p.segments[0].name.clone()
                    }
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                path.push(target_name);
                let mut k = 0;
                while k < ib.methods.len() {
                    check_function(&ib.methods[k], path, current_file, funcs)?;
                    k += 1;
                }
                path.pop();
            }
            Item::Trait(_) => {}
            Item::Use(_) => {}
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
    let mut full = current_module.clone();
    full.push(func.name.clone());
    let expr_types: &Vec<Option<crate::typeck::RType>> =
        if let Some(entry) = func_lookup(funcs, &full) {
            &entry.expr_types
        } else if let Some((_, t)) = template_lookup(funcs, &full) {
            &t.expr_types
        } else {
            unreachable!("typeck registered this function");
        };
    let mut state = SafeState {
        expr_types,
        in_unsafe: false,
        file: current_file.to_string(),
    };
    walk_block(&mut state, &func.body)?;
    Ok(())
}

struct SafeState<'a> {
    expr_types: &'a Vec<Option<crate::typeck::RType>>,
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
            Stmt::Use(_) => {}
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
        ExprKind::IntLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => Ok(()),
        ExprKind::Borrow { inner, .. } => walk_expr(state, inner),
        ExprKind::Cast { inner, .. } => walk_expr(state, inner),
        ExprKind::Deref(inner) => {
            walk_expr(state, inner)?;
            // Inner's resolved type tells us if this is a raw-ptr deref.
            let is_raw = matches!(
                state.expr_types[inner.id as usize].as_ref(),
                Some(crate::typeck::RType::RawPtr { .. })
            );
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
        ExprKind::MethodCall(mc) => walk_method_call(state, mc),
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
        ExprKind::If(if_expr) => {
            walk_expr(state, &if_expr.cond)?;
            walk_block(state, if_expr.then_block.as_ref())?;
            walk_block(state, if_expr.else_block.as_ref())?;
            Ok(())
        }
        ExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr(state, &args[i])?;
                i += 1;
            }
            Ok(())
        }
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                walk_expr(state, &elems[i])?;
                i += 1;
            }
            Ok(())
        }
        ExprKind::TupleIndex { base, .. } => walk_expr(state, base),
        ExprKind::Match(m) => {
            walk_expr(state, &m.scrutinee)?;
            let mut i = 0;
            while i < m.arms.len() {
                walk_expr(state, &m.arms[i].body)?;
                i += 1;
            }
            Ok(())
        }
        ExprKind::IfLet(il) => {
            walk_expr(state, &il.scrutinee)?;
            walk_block(state, il.then_block.as_ref())?;
            walk_block(state, il.else_block.as_ref())?;
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

fn walk_method_call(state: &mut SafeState, mc: &MethodCall) -> Result<(), Error> {
    walk_expr(state, &mc.receiver)?;
    let mut i = 0;
    while i < mc.args.len() {
        walk_expr(state, &mc.args[i])?;
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
