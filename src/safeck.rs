// Safety check pass.
//
// Two rules:
//   1. Dereferencing a raw pointer (`*const T` / `*mut T`) must happen
//      inside an `unsafe { … }` block. Dereferencing a reference
//      (`&T` / `&mut T`) is always safe.
//   2. Calling an `unsafe fn` must happen inside an `unsafe { … }` block.
//
// Inside the body of an `unsafe fn`, both rules are satisfied implicitly
// — the body is treated as if wrapped in `unsafe { … }`.
//
// We don't redo type analysis here — typeck records each Expr's resolved
// type at `FnSymbol.expr_types[expr.id]` and the per-call resolution at
// `FnSymbol.call_resolutions[expr.id]` / `method_resolutions[expr.id]`.
// Safeck reads those.

use crate::ast::{Block, Call, Expr, ExprKind, Function, Item, MethodCall, Module, Stmt, StructLit};
use crate::span::Error;
use crate::typeck::{
    CallResolution, FuncTable, MethodResolution, func_lookup, template_lookup,
};

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
            Item::TypeAlias(_) => {}
            Item::Const(_) => {}
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
    let (expr_types, method_resolutions, call_resolutions): (
        &Vec<Option<crate::typeck::RType>>,
        &Vec<Option<MethodResolution>>,
        &Vec<Option<CallResolution>>,
    ) = if let Some(entry) = func_lookup(funcs, &full) {
        (&entry.expr_types, &entry.method_resolutions, &entry.call_resolutions)
    } else if let Some((_, t)) = template_lookup(funcs, &full) {
        (&t.expr_types, &t.method_resolutions, &t.call_resolutions)
    } else {
        unreachable!("typeck registered this function");
    };
    let mut state = SafeState {
        expr_types,
        method_resolutions,
        call_resolutions,
        funcs,
        // The body of an `unsafe fn` is implicitly inside an unsafe
        // block — raw derefs and unsafe calls don't need an inner
        // `unsafe { … }`.
        in_unsafe: func.is_unsafe,
        file: current_file.to_string(),
    };
    walk_block(&mut state, &func.body)?;
    Ok(())
}

struct SafeState<'a> {
    expr_types: &'a Vec<Option<crate::typeck::RType>>,
    method_resolutions: &'a Vec<Option<MethodResolution>>,
    call_resolutions: &'a Vec<Option<CallResolution>>,
    funcs: &'a FuncTable,
    in_unsafe: bool,
    file: String,
}

fn walk_block(state: &mut SafeState, block: &Block) -> Result<(), Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                if let Some(v) = &let_stmt.value {
                    walk_expr(state, v)?;
                }
            }
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
        ExprKind::IntLit(_) | ExprKind::NegIntLit(_) | ExprKind::StrLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::Var(_) => Ok(()),
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
        ExprKind::Call(call) => walk_call(state, call, expr),
        ExprKind::MethodCall(mc) => walk_method_call(state, mc, expr),
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
        ExprKind::While(w) => {
            walk_expr(state, &w.cond)?;
            walk_block(state, w.body.as_ref())
        }
        ExprKind::For(f) => {
            walk_expr(state, &f.iter)?;
            walk_block(state, f.body.as_ref())
        }
        ExprKind::Break { .. } | ExprKind::Continue { .. } => Ok(()),
        ExprKind::Return { value } => {
            if let Some(v) = value {
                walk_expr(state, v)?;
            }
            Ok(())
        }
        ExprKind::Try { inner, .. } => walk_expr(state, inner),
        ExprKind::Index { base, index, .. } => {
            walk_expr(state, base)?;
            walk_expr(state, index)
        }
        ExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr(state, &args[i])?;
                i += 1;
            }
            Ok(())
        }
        ExprKind::Closure(_) => {
            unreachable!("closure expressions rejected at typeck before safeck")
        }
    }
}

fn walk_call(state: &mut SafeState, call: &Call, expr: &Expr) -> Result<(), Error> {
    // Resolve the call to its callee FnSymbol/Template and check
    // unsafe-ness. Variant constructors aren't unsafe-callable.
    let callee_unsafe = match state.call_resolutions[expr.id as usize].as_ref() {
        Some(CallResolution::Direct(idx)) => state.funcs.entries[*idx].is_unsafe,
        Some(CallResolution::Generic { template_idx, .. }) => {
            state.funcs.templates[*template_idx].is_unsafe
        }
        // Indirect calls don't carry an `unsafe` marker on the FnPtr
        // type today (no `unsafe fn(...) -> R` syntax). When that lands,
        // route the unsafe bit through the FnPtr's type and re-check
        // here. For now: treat as safe.
        Some(CallResolution::Variant { .. })
        | Some(CallResolution::Indirect { .. })
        | None => false,
    };
    if callee_unsafe && !state.in_unsafe {
        return Err(Error {
            file: state.file.clone(),
            message: "call to unsafe function requires an `unsafe` block".to_string(),
            span: expr.span.copy(),
        });
    }
    let mut i = 0;
    while i < call.args.len() {
        walk_expr(state, &call.args[i])?;
        i += 1;
    }
    Ok(())
}

fn walk_method_call(state: &mut SafeState, mc: &MethodCall, expr: &Expr) -> Result<(), Error> {
    let callee_unsafe = match state.method_resolutions[expr.id as usize].as_ref() {
        Some(res) => match res.template_idx {
            Some(idx) => state.funcs.templates[idx].is_unsafe,
            None => {
                // Direct call — find the FnSymbol whose wasm idx matches.
                let mut found = false;
                let mut k = 0;
                while k < state.funcs.entries.len() {
                    if state.funcs.entries[k].idx == res.callee_idx {
                        found = state.funcs.entries[k].is_unsafe;
                        break;
                    }
                    k += 1;
                }
                found
            }
        },
        None => false,
    };
    if callee_unsafe && !state.in_unsafe {
        return Err(Error {
            file: state.file.clone(),
            message: "call to unsafe method requires an `unsafe` block".to_string(),
            span: expr.span.copy(),
        });
    }
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
