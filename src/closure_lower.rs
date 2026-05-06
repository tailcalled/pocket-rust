// Closure-lowering pass. Runs after typeck (so each closure's
// resolved param/return types are recorded on FnSymbol/Template's
// `closures` side table) and before borrowck. Walks every function
// body in the Module tree, replacing `ExprKind::Closure` with a unit
// struct literal of the synthesized `__closure_<id>` type. For each
// closure replaced, synthesizes a matching `Item::Impl Fn<(P0, P1,
// ...)> for __closure_<id>` whose method body is the original closure
// body, deep-cloned into a fresh NodeId space and prepended by
// `let pN = args.N;` for each closure param.
//
// Phase 1A scope: non-capturing closures only (typeck rejects
// captures upstream). One `Fn` impl per closure — `FnMut` and
// `FnOnce` mirrors come in phase 3 with capture-mode inference.

use crate::ast::{
    Block, Expr, ExprKind, FieldInit, FieldPattern, Function, IfExpr, IfLetExpr, Item, LetStmt,
    Lifetime, MatchArm, MatchExpr, MethodCall, Module, NodeId, Param, Path, PathSegment,
    Pattern, PatternKind, Stmt, Type, TypeKind, AssignStmt, AssocConstraint, Call, FieldAccess,
    ImplAssocType, ImplBlock, StructLit, TraitBound, ForLoop, WhileExpr,
};
use crate::span::{Error, Pos, Span};
use crate::typeck::{
    self, CaptureMode, ClosureInfo, FuncTable, RType, ReExportTable, StructTable, TraitTable,
};

// Public entry point. Walks `module` in place, rewrites every
// `ExprKind::Closure` into a struct lit, appends synthesized
// `Item::Impl` nodes for each closure, and registers the new impls +
// methods in the existing tables. After this returns the AST contains
// no closure expressions and borrowck/safeck/codegen can run normally.
pub fn lower(
    module: &mut Module,
    structs: &mut StructTable,
    enums: &mut typeck::EnumTable,
    aliases: &mut typeck::AliasTable,
    traits: &mut TraitTable,
    funcs: &mut FuncTable,
    reexports: &mut ReExportTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    let path: Vec<String> = Vec::new();
    let mut path = path;
    walk_module(module, &mut path, structs, enums, aliases, traits, funcs, reexports, next_idx)?;
    Ok(())
}

fn walk_module(
    module: &mut Module,
    path: &mut Vec<String>,
    structs: &mut StructTable,
    enums: &mut typeck::EnumTable,
    aliases: &mut typeck::AliasTable,
    traits: &mut TraitTable,
    funcs: &mut FuncTable,
    reexports: &mut ReExportTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    if !module.name.is_empty() {
        path.push(module.name.clone());
    }
    // First, recurse into submodules — their closures get rewritten
    // first so the parent module's walk only deals with its own
    // direct functions/impls.
    let mut i = 0;
    while i < module.items.len() {
        if let Item::Module(m) = &mut module.items[i] {
            walk_module(m, path, structs, enums, aliases, traits, funcs, reexports, next_idx)?;
        }
        i += 1;
    }
    // Collect synthesized items + rewrite closure expressions in each
    // function body in this module.
    let mut new_items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < module.items.len() {
        match &mut module.items[i] {
            Item::Function(f) => {
                let mut full = path.clone();
                full.push(f.name.clone());
                process_fn(f, &full, &module.source_file, &mut new_items, funcs)?;
            }
            Item::Impl(ib) => {
                let target_seg = match &ib.target.kind {
                    TypeKind::Path(p) if !p.segments.is_empty() => {
                        Some(p.segments[p.segments.len() - 1].name.clone())
                    }
                    _ => None,
                };
                let mut k = 0;
                while k < ib.methods.len() {
                    let mut full = path.clone();
                    if let Some(seg) = &target_seg {
                        full.push(seg.clone());
                    }
                    full.push(ib.methods[k].name.clone());
                    process_fn(&mut ib.methods[k], &full, &module.source_file, &mut new_items, funcs)?;
                    k += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Register synthesized items in tables, then append to module.
    let mut i = 0;
    while i < new_items.len() {
        if let Item::Impl(ib) = &new_items[i] {
            register_synthesized_impl(
                ib,
                path,
                &module.source_file,
                structs,
                enums,
                aliases,
                traits,
                funcs,
                reexports,
                next_idx,
            )?;
        }
        i += 1;
    }
    // Append synthesized items to the module IN THE SAME ORDER they
    // were registered with `register_synthesized_impl` above. This
    // keeps FnSymbol.idx (assigned at registration time) in sync with
    // codegen's emission order (module.items walk), which assigns the
    // matching wasm function index. Reversing here would put
    // FnOnce/Fn out of order and codegen would dispatch via the wrong
    // wasm idx for closures with multiple impls.
    for it in new_items {
        module.items.push(it);
    }
    if !module.name.is_empty() {
        path.pop();
    }
    Ok(())
}

// Process a single function: read its closures from FuncTable (matched
// by `fn_path`), walk its body to rewrite closures into struct lits,
// and emit synthesized impl Items into `out`.
fn process_fn(
    func: &mut Function,
    fn_path: &Vec<String>,
    source_file: &str,
    out: &mut Vec<Item>,
    funcs: &FuncTable,
) -> Result<(), Error> {
    // Look up the function's closures vector (entries first, then
    // templates).
    let closures: Vec<Option<ClosureInfo>> = lookup_fn_closures(funcs, fn_path);
    if closures.is_empty() {
        // No closures recorded — nothing to do (also handles the case
        // where the function isn't in the table, e.g. trait method
        // signatures).
        return Ok(());
    }
    rewrite_block(&mut func.body, &closures, source_file, out)?;
    Ok(())
}

fn lookup_fn_closures(funcs: &FuncTable, fn_path: &Vec<String>) -> Vec<Option<ClosureInfo>> {
    let mut e = 0;
    while e < funcs.entries.len() {
        if &funcs.entries[e].path == fn_path {
            return funcs.entries[e].closures.clone();
        }
        e += 1;
    }
    let mut t = 0;
    while t < funcs.templates.len() {
        if &funcs.templates[t].path == fn_path {
            return funcs.templates[t].closures.clone();
        }
        t += 1;
    }
    Vec::new()
}

fn rewrite_block(
    block: &mut Block,
    closures: &Vec<Option<ClosureInfo>>,
    source_file: &str,
    out: &mut Vec<Item>,
) -> Result<(), Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        rewrite_stmt(&mut block.stmts[i], closures, source_file, out)?;
        i += 1;
    }
    if let Some(tail) = &mut block.tail {
        rewrite_expr(tail, closures, source_file, out)?;
    }
    Ok(())
}

fn rewrite_stmt(
    stmt: &mut Stmt,
    closures: &Vec<Option<ClosureInfo>>,
    source_file: &str,
    out: &mut Vec<Item>,
) -> Result<(), Error> {
    match stmt {
        Stmt::Let(ls) => {
            rewrite_expr(&mut ls.value, closures, source_file, out)?;
            if let Some(eb) = &mut ls.else_block {
                rewrite_block(eb, closures, source_file, out)?;
            }
        }
        Stmt::Assign(AssignStmt { lhs, rhs, .. }) => {
            rewrite_expr(lhs, closures, source_file, out)?;
            rewrite_expr(rhs, closures, source_file, out)?;
        }
        Stmt::Expr(e) => rewrite_expr(e, closures, source_file, out)?,
        Stmt::Use(_) => {}
    }
    Ok(())
}

fn rewrite_expr(
    expr: &mut Expr,
    closures: &Vec<Option<ClosureInfo>>,
    source_file: &str,
    out: &mut Vec<Item>,
) -> Result<(), Error> {
    // Recurse first so nested closures get rewritten before their
    // parent. This guarantees that when we synthesize the parent's
    // impl method body, any inner closures inside it have already been
    // turned into struct lits.
    match &mut expr.kind {
        ExprKind::IntLit(_)
        | ExprKind::NegIntLit(_)
        | ExprKind::StrLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::Var(_)
        | ExprKind::Break { .. }
        | ExprKind::Continue { .. } => {}
        ExprKind::Borrow { inner, .. } => rewrite_expr(inner, closures, source_file, out)?,
        ExprKind::Cast { inner, .. } => rewrite_expr(inner, closures, source_file, out)?,
        ExprKind::Deref(inner) => rewrite_expr(inner, closures, source_file, out)?,
        ExprKind::Block(b) => rewrite_block(b, closures, source_file, out)?,
        ExprKind::Unsafe(b) => rewrite_block(b, closures, source_file, out)?,
        ExprKind::FieldAccess(fa) => rewrite_expr(&mut fa.base, closures, source_file, out)?,
        ExprKind::TupleIndex { base, .. } => rewrite_expr(base, closures, source_file, out)?,
        ExprKind::Tuple(elems) => {
            let mut k = 0;
            while k < elems.len() {
                rewrite_expr(&mut elems[k], closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::Builtin { args, .. } => {
            let mut k = 0;
            while k < args.len() {
                rewrite_expr(&mut args[k], closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::Call(c) => {
            let mut k = 0;
            while k < c.args.len() {
                rewrite_expr(&mut c.args[k], closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::MethodCall(mc) => {
            rewrite_expr(&mut mc.receiver, closures, source_file, out)?;
            let mut k = 0;
            while k < mc.args.len() {
                rewrite_expr(&mut mc.args[k], closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::StructLit(lit) => {
            let mut k = 0;
            while k < lit.fields.len() {
                rewrite_expr(&mut lit.fields[k].value, closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::If(ie) => {
            rewrite_expr(&mut ie.cond, closures, source_file, out)?;
            rewrite_block(&mut ie.then_block, closures, source_file, out)?;
            rewrite_block(&mut ie.else_block, closures, source_file, out)?;
        }
        ExprKind::IfLet(il) => {
            rewrite_expr(&mut il.scrutinee, closures, source_file, out)?;
            rewrite_block(&mut il.then_block, closures, source_file, out)?;
            rewrite_block(&mut il.else_block, closures, source_file, out)?;
        }
        ExprKind::Match(m) => {
            rewrite_expr(&mut m.scrutinee, closures, source_file, out)?;
            let mut k = 0;
            while k < m.arms.len() {
                if let Some(g) = &mut m.arms[k].guard {
                    rewrite_expr(g, closures, source_file, out)?;
                }
                rewrite_expr(&mut m.arms[k].body, closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::While(w) => {
            rewrite_expr(&mut w.cond, closures, source_file, out)?;
            rewrite_block(&mut w.body, closures, source_file, out)?;
        }
        ExprKind::For(f) => {
            rewrite_expr(&mut f.iter, closures, source_file, out)?;
            rewrite_block(&mut f.body, closures, source_file, out)?;
        }
        ExprKind::Return { value } => {
            if let Some(v) = value {
                rewrite_expr(v, closures, source_file, out)?;
            }
        }
        ExprKind::Try { inner, .. } => rewrite_expr(inner, closures, source_file, out)?,
        ExprKind::Index { base, index, .. } => {
            rewrite_expr(base, closures, source_file, out)?;
            rewrite_expr(index, closures, source_file, out)?;
        }
        ExprKind::MacroCall { args, .. } => {
            let mut k = 0;
            while k < args.len() {
                rewrite_expr(&mut args[k], closures, source_file, out)?;
                k += 1;
            }
        }
        ExprKind::Closure(_) => {} // handled below
    }
    // Now check if this expression itself is a closure, and if so,
    // rewrite it.
    if let ExprKind::Closure(_) = &expr.kind {
        let id = expr.id as usize;
        let info = match closures.get(id).and_then(|c| c.as_ref()) {
            Some(ci) => ci.clone(),
            None => {
                return Err(Error {
                    file: source_file.to_string(),
                    message: "internal: closure expression has no recorded ClosureInfo"
                        .to_string(),
                    span: expr.span.copy(),
                });
            }
        };
        // Take the closure out so we can move its body without cloning.
        let placeholder = Expr {
            kind: ExprKind::Tuple(Vec::new()),
            span: expr.span.copy(),
            id: expr.id,
        };
        let old = std::mem::replace(expr, placeholder);
        let closure = match old.kind {
            ExprKind::Closure(c) => c,
            _ => unreachable!(),
        };
        // Synthesize impls. Non-`move` closures get all three (Fn,
        // FnMut, FnOnce) so the supertrait-chain validation passes
        // and callers can dispatch through any of the family methods;
        // `move` closures get FnOnce only (they consume captures, so
        // multi-call via `&self`/`&mut self` would be unsound). Each
        // impl gets its own deep-clone of the body in a fresh NodeId
        // space.
        let impls = synthesize_impls_for_closure(&info, &closure, source_file)?;
        for ib in impls {
            out.push(Item::Impl(ib));
        }
        let _ = closure;
        // Replace the expression with `__closure_<id> {}` (empty struct
        // literal of the synthesized unit struct).
        let synth_path_segments: Vec<PathSegment> = info
            .synthesized_struct_path
            .iter()
            .map(|s| PathSegment {
                name: s.clone(),
                span: expr.span.copy(),
                lifetime_args: Vec::new(),
                args: Vec::new(),
            })
            .collect();
        let synth_path = Path {
            segments: synth_path_segments,
            span: expr.span.copy(),
        };
        // Build one struct-literal field per capture, initializing
        // each from the captured binding at the closure expression
        // site. Move captures (Copy types stored by value) emit a
        // plain `Var(name)`. Ref/RefMut captures wrap that in `&` /
        // `&mut` so the field, which has type `&'cap T`, gets the
        // borrow of the outer binding. Borrowck/codegen lookup
        // `Var(name)` against the function's locals stack rather than
        // via per-NodeId tables, so the synthesized Var nodes can
        // carry placeholder NodeIds (id: 0) without conflict.
        let mut field_inits: Vec<FieldInit> = Vec::new();
        let mut c = 0;
        while c < info.captures.len() {
            let name = info.captures[c].binding_name.clone();
            let var_expr = Expr {
                kind: ExprKind::Var(name.clone()),
                span: expr.span.copy(),
                id: 0,
            };
            let value = match info.captures[c].mode {
                CaptureMode::Move => var_expr,
                CaptureMode::Ref => Expr {
                    kind: ExprKind::Borrow {
                        inner: Box::new(var_expr),
                        mutable: false,
                    },
                    span: expr.span.copy(),
                    id: 0,
                },
                CaptureMode::RefMut => Expr {
                    kind: ExprKind::Borrow {
                        inner: Box::new(var_expr),
                        mutable: true,
                    },
                    span: expr.span.copy(),
                    id: 0,
                },
            };
            field_inits.push(FieldInit {
                name: name.clone(),
                name_span: expr.span.copy(),
                value,
            });
            c += 1;
        }
        expr.kind = ExprKind::StructLit(StructLit {
            path: synth_path,
            fields: field_inits,
        });
    }
    Ok(())
}

// Family of Fn-trait impl flavors a closure may need.
#[derive(Clone, Copy)]
enum FnFamily {
    Fn,
    FnMut,
    FnOnce,
}

// Top-level: produce impl blocks for one closure. The trait set is
// driven by `move` keyword + body mutation analysis:
//   `move`                            → FnOnce only
//   non-move, mutates a capture       → FnMut + FnOnce
//   non-move, read-only               → Fn + FnMut + FnOnce
// FnOnce always comes first in the output Vec — Fn/FnMut signature
// validation has to resolve `Self::Output` via the supertrait chain,
// which requires FnOnce's `type Output = R` binding to be registered
// before Fn/FnMut's signatures are validated.
fn synthesize_impls_for_closure(
    info: &ClosureInfo,
    closure: &crate::ast::Closure,
    source_file: &str,
) -> Result<Vec<ImplBlock>, Error> {
    let mut out: Vec<ImplBlock> = Vec::new();
    out.push(synthesize_impl_for_closure(info, closure, FnFamily::FnOnce, source_file)?);
    if !info.is_move {
        out.push(synthesize_impl_for_closure(info, closure, FnFamily::FnMut, source_file)?);
        if !info.body_mutates_capture {
            out.push(synthesize_impl_for_closure(info, closure, FnFamily::Fn, source_file)?);
        }
    }
    Ok(out)
}

// Build an `impl <Trait><(P0, P1, ...)> for __closure_<id>` AST. Trait
// is one of Fn/FnMut/FnOnce per `family`; method body is `let p0 =
// args.0; ...; <closure body>` cloned into a fresh NodeId space.
fn synthesize_impl_for_closure(
    info: &ClosureInfo,
    closure: &crate::ast::Closure,
    family: FnFamily,
    source_file: &str,
) -> Result<ImplBlock, Error> {
    // Each Fn-family impl for the same closure shares the same source
    // (`info.body_span`). `find_trait_impl_idx_by_span` keys on
    // `(file, start.line, start.col)`, so without disambiguation the
    // three impls would collide and borrowck/codegen would all map to
    // the first row. Bump start.col by a per-family offset so each
    // impl's span is unique while still pointing inside the closure
    // body for any error attributions.
    let family_offset: u32 = match family {
        FnFamily::Fn => 0,
        FnFamily::FnMut => 1,
        FnFamily::FnOnce => 2,
    };
    let base = info.body_span.copy();
    let span = Span::new(
        Pos::new(base.start.line, base.start.col + family_offset),
        base.end.copy(),
    );
    let mut id_alloc = NodeIdAllocator { next: 0 };

    // Build the Fn-family trait path: `<Trait><(P0, P1, ...), Output = R>`.
    let arg_tuple_ty = Type {
        kind: TypeKind::Tuple(
            info.param_types
                .iter()
                .map(|rt| rtype_to_ast_type(rt, &span, source_file))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        span: span.copy(),
    };
    let output_ty = rtype_to_ast_type(&info.return_type, &span, source_file)?;
    // Trait + method names per family.
    let (trait_name, method_name) = match family {
        FnFamily::Fn => ("Fn", "call"),
        FnFamily::FnMut => ("FnMut", "call_mut"),
        FnFamily::FnOnce => ("FnOnce", "call_once"),
    };
    // Fully-qualified `std::ops::<Trait>` so resolution doesn't depend
    // on the surrounding module's use_scope (synthesized impls are
    // added post-typeck and don't carry the closure's lexical use
    // scope).
    let trait_path = Path {
        segments: vec![
            PathSegment {
                name: "std".to_string(),
                span: span.copy(),
                lifetime_args: Vec::new(),
                args: Vec::new(),
            },
            PathSegment {
                name: "ops".to_string(),
                span: span.copy(),
                lifetime_args: Vec::new(),
                args: Vec::new(),
            },
            PathSegment {
                name: trait_name.to_string(),
                span: span.copy(),
                lifetime_args: Vec::new(),
                args: vec![arg_tuple_ty.clone()],
            },
        ],
        span: span.copy(),
    };

    // Target: `__closure_<id>` (path-qualified to its module). When the
    // synthesized struct carries a `'cap` lifetime parameter (because
    // at least one capture is by-ref), the impl's target supplies a
    // single lifetime arg referencing the impl's `'cap` parameter.
    let needs_cap_lifetime = info
        .captures
        .iter()
        .any(|c| !matches!(c.mode, CaptureMode::Move));
    let mut target_segments: Vec<PathSegment> = info
        .synthesized_struct_path
        .iter()
        .map(|s| PathSegment {
            name: s.clone(),
            span: span.copy(),
            lifetime_args: Vec::new(),
            args: Vec::new(),
        })
        .collect();
    if needs_cap_lifetime {
        let last = target_segments.len() - 1;
        target_segments[last].lifetime_args = vec![Lifetime {
            name: "cap".to_string(),
            span: span.copy(),
        }];
    }
    let target_ty = Type {
        kind: TypeKind::Path(Path {
            segments: target_segments,
            span: span.copy(),
        }),
        span: span.copy(),
    };
    let impl_lifetime_params = if needs_cap_lifetime {
        vec![crate::ast::LifetimeParam {
            name: "cap".to_string(),
            name_span: span.copy(),
        }]
    } else {
        Vec::new()
    };

    // Self-param shape per family: `&self` for Fn, `&mut self` for
    // FnMut, owned `self` for FnOnce. Mirrors the trait method's
    // declared receiver shape so signature validation matches.
    let self_param = match family {
        FnFamily::Fn => Param {
            name: "self".to_string(),
            name_span: span.copy(),
            ty: Type {
                kind: TypeKind::Ref {
                    inner: Box::new(Type {
                        kind: TypeKind::SelfType,
                        span: span.copy(),
                    }),
                    mutable: false,
                    lifetime: None,
                },
                span: span.copy(),
            },
        },
        FnFamily::FnMut => Param {
            name: "self".to_string(),
            name_span: span.copy(),
            ty: Type {
                kind: TypeKind::Ref {
                    inner: Box::new(Type {
                        kind: TypeKind::SelfType,
                        span: span.copy(),
                    }),
                    mutable: true,
                    lifetime: None,
                },
                span: span.copy(),
            },
        },
        FnFamily::FnOnce => Param {
            name: "self".to_string(),
            name_span: span.copy(),
            ty: Type {
                kind: TypeKind::SelfType,
                span: span.copy(),
            },
        },
    };
    let args_param = Param {
        name: "__args".to_string(),
        name_span: span.copy(),
        ty: arg_tuple_ty.clone(),
    };

    // Build the body: `let p0 = __args.0; let p1 = __args.1; ...; <closure body>`.
    let mut stmts: Vec<Stmt> = Vec::new();
    let mut idx = 0u32;
    while (idx as usize) < closure.params.len() {
        let pname = closure.params[idx as usize].name.clone();
        let pname_span = closure.params[idx as usize].name_span.copy();
        let args_var = mk_expr(&mut id_alloc, ExprKind::Var("__args".to_string()), &span);
        let tuple_idx = Expr {
            kind: ExprKind::TupleIndex {
                base: Box::new(args_var),
                index: idx,
                index_span: span.copy(),
            },
            span: span.copy(),
            id: id_alloc.alloc(),
        };
        let pat = Pattern {
            kind: PatternKind::Binding {
                name: pname.clone(),
                name_span: pname_span.copy(),
                by_ref: false,
                mutable: false,
            },
            span: pname_span.copy(),
            id: id_alloc.alloc(),
        };
        stmts.push(Stmt::Let(LetStmt {
            pattern: pat,
            ty: None,
            value: tuple_idx,
            else_block: None,
        }));
        idx += 1;
    }
    // Build the (name, mode) list — used by the deep-clone walk to
    // rewrite each `Var(name)` reference in the body. For Move
    // captures (Copy types stored by value) the rewrite is
    // `self.<name>`; for Ref/RefMut captures (the field is `&T`/
    // `&mut T`), the rewrite is `*self.<name>` so the body sees the
    // captured T as a place. Phase 2A/B: simple lexical rewriting —
    // inner shadowing of a captured name would mis-rewrite, but no
    // in-tree closure trips that case yet.
    let capture_modes: Vec<(String, CaptureMode)> = info
        .captures
        .iter()
        .map(|c| (c.binding_name.clone(), c.mode))
        .collect();

    // For FnOnce on a mutating closure body: pocket-rust function
    // params are immutable bindings, so `self.x = ...` directly on
    // the by-value `self` param fails the place-mutability check.
    // Shadow `self` into a `mut __closure_self` local at the top of
    // the body and rewrite captures against that name instead. Other
    // family/mode combos use `self` directly (FnMut/Fn dispatch via
    // `&mut self`/`&self` where field assignment to a Copy capture
    // works through the ref).
    let needs_mut_rebind = matches!(family, FnFamily::FnOnce) && info.body_mutates_capture;
    let receiver_binding_name = if needs_mut_rebind {
        "__closure_self".to_string()
    } else {
        "self".to_string()
    };
    if needs_mut_rebind {
        let self_var = mk_expr(&mut id_alloc, ExprKind::Var("self".to_string()), &span);
        let pat = Pattern {
            kind: PatternKind::Binding {
                name: "__closure_self".to_string(),
                name_span: span.copy(),
                by_ref: false,
                mutable: true,
            },
            span: span.copy(),
            id: id_alloc.alloc(),
        };
        stmts.push(Stmt::Let(LetStmt {
            pattern: pat,
            ty: None,
            value: self_var,
            else_block: None,
        }));
    }
    // Deep-clone the closure body into the new NodeId space, rewriting
    // captures along the way against the chosen receiver binding name.
    let body = clone_expr_fresh_ids(
        &closure.body,
        &mut id_alloc,
        &capture_modes,
        &receiver_binding_name,
    );
    let block = Block {
        stmts,
        tail: Some(body),
        span: span.copy(),
    };

    let method = Function {
        name: method_name.to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![self_param, args_param],
        return_type: Some(output_ty.clone()),
        body: block,
        node_count: id_alloc.next,
        is_pub: true,
        is_unsafe: false,
    };

    // Only FnOnce declares `type Output;` in the trait family —
    // FnMut and Fn inherit it via the supertrait chain. The impl-side
    // `type Output = R` binding therefore goes only on the FnOnce
    // impl; FnMut/Fn impls leave assoc_type_bindings empty.
    let assoc_type_bindings = match family {
        FnFamily::FnOnce => vec![ImplAssocType {
            name: "Output".to_string(),
            name_span: span.copy(),
            ty: output_ty,
        }],
        FnFamily::Fn | FnFamily::FnMut => Vec::new(),
    };

    Ok(ImplBlock {
        lifetime_params: impl_lifetime_params,
        type_params: Vec::new(),
        trait_path: Some(trait_path),
        target: target_ty,
        methods: vec![method],
        assoc_type_bindings,
        span,
    })
}

// Allocator for fresh NodeIds inside a synthesized function body.
struct NodeIdAllocator {
    next: NodeId,
}

impl NodeIdAllocator {
    fn alloc(&mut self) -> NodeId {
        let id = self.next;
        self.next += 1;
        id
    }
}

fn mk_expr(alloc: &mut NodeIdAllocator, kind: ExprKind, span: &Span) -> Expr {
    Expr {
        kind,
        span: span.copy(),
        id: alloc.alloc(),
    }
}

// Deep-clone an Expr tree, allocating fresh NodeIds for every Expr and
// Pattern node so the result lives in a new function's NodeId space.
// `captures` carries the names of bindings the closure body referenced
// from outside its scope; each `Var(name)` whose name appears in
// `captures` is rewritten to `(self.<name>)` so the synthesized impl
// method's body reads the capture from the closure-struct field. Phase
// 2A: simple lexical rewrite — inner-let shadowing of a captured name
// would mis-rewrite the inner reference, but no in-tree closure trips
// that case yet.
fn clone_expr_fresh_ids(e: &Expr, alloc: &mut NodeIdAllocator, captures: &[(String, CaptureMode)], recv_name: &str) -> Expr {
    // `Var(name)` for a captured outer binding becomes
    // `self.<name>` (Move/Copy capture) or `*self.<name>` (Ref/RefMut
    // capture, where the field is `&T`/`&mut T` and we deref to the
    // underlying place).
    if let ExprKind::Var(name) = &e.kind {
        if let Some((_, mode)) = captures.iter().find(|(n, _)| n == name) {
            let self_var = mk_expr(alloc, ExprKind::Var(recv_name.to_string()), &e.span);
            let field_access = Expr {
                kind: ExprKind::FieldAccess(FieldAccess {
                    base: Box::new(self_var),
                    field: name.clone(),
                    field_span: e.span.copy(),
                }),
                span: e.span.copy(),
                id: alloc.alloc(),
            };
            let kind = match mode {
                CaptureMode::Move => return field_access,
                CaptureMode::Ref | CaptureMode::RefMut => {
                    ExprKind::Deref(Box::new(field_access))
                }
            };
            return Expr {
                kind,
                span: e.span.copy(),
                id: alloc.alloc(),
            };
        }
    }
    let kind = match &e.kind {
        ExprKind::IntLit(n) => ExprKind::IntLit(*n),
        ExprKind::NegIntLit(n) => ExprKind::NegIntLit(*n),
        ExprKind::StrLit(s) => ExprKind::StrLit(s.clone()),
        ExprKind::BoolLit(b) => ExprKind::BoolLit(*b),
        ExprKind::CharLit(c) => ExprKind::CharLit(*c),
        ExprKind::Var(n) => ExprKind::Var(n.clone()),
        ExprKind::Borrow { inner, mutable } => ExprKind::Borrow {
            inner: Box::new(clone_expr_fresh_ids(inner, alloc, captures, recv_name)),
            mutable: *mutable,
        },
        ExprKind::Cast { inner, ty } => ExprKind::Cast {
            inner: Box::new(clone_expr_fresh_ids(inner, alloc, captures, recv_name)),
            ty: ty.clone(),
        },
        ExprKind::Deref(inner) => ExprKind::Deref(Box::new(clone_expr_fresh_ids(inner, alloc, captures, recv_name))),
        ExprKind::Block(b) => ExprKind::Block(Box::new(clone_block_fresh_ids(b, alloc, captures, recv_name))),
        ExprKind::Unsafe(b) => ExprKind::Unsafe(Box::new(clone_block_fresh_ids(b, alloc, captures, recv_name))),
        ExprKind::FieldAccess(fa) => ExprKind::FieldAccess(FieldAccess {
            base: Box::new(clone_expr_fresh_ids(&fa.base, alloc, captures, recv_name)),
            field: fa.field.clone(),
            field_span: fa.field_span.copy(),
        }),
        ExprKind::TupleIndex { base, index, index_span } => ExprKind::TupleIndex {
            base: Box::new(clone_expr_fresh_ids(base, alloc, captures, recv_name)),
            index: *index,
            index_span: index_span.copy(),
        },
        ExprKind::Tuple(elems) => ExprKind::Tuple(
            elems
                .iter()
                .map(|e| clone_expr_fresh_ids(e, alloc, captures, recv_name))
                .collect(),
        ),
        ExprKind::Builtin { name, name_span, type_args, args } => ExprKind::Builtin {
            name: name.clone(),
            name_span: name_span.copy(),
            type_args: type_args.clone(),
            args: args.iter().map(|a| clone_expr_fresh_ids(a, alloc, captures, recv_name)).collect(),
        },
        ExprKind::Call(c) => ExprKind::Call(Call {
            callee: c.callee.clone(),
            args: c.args.iter().map(|a| clone_expr_fresh_ids(a, alloc, captures, recv_name)).collect(),
        }),
        ExprKind::MethodCall(mc) => ExprKind::MethodCall(MethodCall {
            receiver: Box::new(clone_expr_fresh_ids(&mc.receiver, alloc, captures, recv_name)),
            method: mc.method.clone(),
            method_span: mc.method_span.copy(),
            turbofish_args: mc.turbofish_args.clone(),
            args: mc.args.iter().map(|a| clone_expr_fresh_ids(a, alloc, captures, recv_name)).collect(),
        }),
        ExprKind::StructLit(lit) => ExprKind::StructLit(StructLit {
            path: lit.path.clone(),
            fields: lit
                .fields
                .iter()
                .map(|f| FieldInit {
                    name: f.name.clone(),
                    name_span: f.name_span.copy(),
                    value: clone_expr_fresh_ids(&f.value, alloc, captures, recv_name),
                })
                .collect(),
        }),
        ExprKind::If(ie) => ExprKind::If(IfExpr {
            cond: Box::new(clone_expr_fresh_ids(&ie.cond, alloc, captures, recv_name)),
            then_block: Box::new(clone_block_fresh_ids(&ie.then_block, alloc, captures, recv_name)),
            else_block: Box::new(clone_block_fresh_ids(&ie.else_block, alloc, captures, recv_name)),
        }),
        ExprKind::IfLet(il) => ExprKind::IfLet(IfLetExpr {
            pattern: clone_pattern_fresh_ids(&il.pattern, alloc),
            scrutinee: Box::new(clone_expr_fresh_ids(&il.scrutinee, alloc, captures, recv_name)),
            then_block: Box::new(clone_block_fresh_ids(&il.then_block, alloc, captures, recv_name)),
            else_block: Box::new(clone_block_fresh_ids(&il.else_block, alloc, captures, recv_name)),
        }),
        ExprKind::Match(m) => ExprKind::Match(MatchExpr {
            scrutinee: Box::new(clone_expr_fresh_ids(&m.scrutinee, alloc, captures, recv_name)),
            arms: m
                .arms
                .iter()
                .map(|a| MatchArm {
                    pattern: clone_pattern_fresh_ids(&a.pattern, alloc),
                    guard: a.guard.as_ref().map(|g| clone_expr_fresh_ids(g, alloc, captures, recv_name)),
                    body: clone_expr_fresh_ids(&a.body, alloc, captures, recv_name),
                    span: a.span.copy(),
                })
                .collect(),
            span: m.span.copy(),
        }),
        ExprKind::While(w) => ExprKind::While(WhileExpr {
            label: w.label.clone(),
            label_span: w.label_span.as_ref().map(|s| s.copy()),
            cond: Box::new(clone_expr_fresh_ids(&w.cond, alloc, captures, recv_name)),
            body: Box::new(clone_block_fresh_ids(&w.body, alloc, captures, recv_name)),
        }),
        ExprKind::For(f) => ExprKind::For(ForLoop {
            label: f.label.clone(),
            label_span: f.label_span.as_ref().map(|s| s.copy()),
            pattern: clone_pattern_fresh_ids(&f.pattern, alloc),
            iter: Box::new(clone_expr_fresh_ids(&f.iter, alloc, captures, recv_name)),
            body: Box::new(clone_block_fresh_ids(&f.body, alloc, captures, recv_name)),
        }),
        ExprKind::Break { label, label_span } => ExprKind::Break {
            label: label.clone(),
            label_span: label_span.as_ref().map(|s| s.copy()),
        },
        ExprKind::Continue { label, label_span } => ExprKind::Continue {
            label: label.clone(),
            label_span: label_span.as_ref().map(|s| s.copy()),
        },
        ExprKind::Return { value } => ExprKind::Return {
            value: value.as_ref().map(|v| Box::new(clone_expr_fresh_ids(v, alloc, captures, recv_name))),
        },
        ExprKind::Try { inner, question_span } => ExprKind::Try {
            inner: Box::new(clone_expr_fresh_ids(inner, alloc, captures, recv_name)),
            question_span: question_span.copy(),
        },
        ExprKind::Index { base, index, bracket_span } => ExprKind::Index {
            base: Box::new(clone_expr_fresh_ids(base, alloc, captures, recv_name)),
            index: Box::new(clone_expr_fresh_ids(index, alloc, captures, recv_name)),
            bracket_span: bracket_span.copy(),
        },
        ExprKind::MacroCall { name, name_span, args } => ExprKind::MacroCall {
            name: name.clone(),
            name_span: name_span.copy(),
            args: args.iter().map(|a| clone_expr_fresh_ids(a, alloc, captures, recv_name)).collect(),
        },
        ExprKind::Closure(_) => {
            // Inner closures should already have been rewritten by
            // `rewrite_expr` before we cloned this body. If we reach
            // this arm something's gone wrong; preserve the node for
            // the typeck stage to surface a clearer error.
            unreachable!("inner closures must be rewritten before clone_expr_fresh_ids")
        }
    };
    Expr {
        kind,
        span: e.span.copy(),
        id: alloc.alloc(),
    }
}

fn clone_block_fresh_ids(b: &Block, alloc: &mut NodeIdAllocator, captures: &[(String, CaptureMode)], recv_name: &str) -> Block {
    let mut stmts: Vec<Stmt> = Vec::new();
    let mut i = 0;
    while i < b.stmts.len() {
        stmts.push(clone_stmt_fresh_ids(&b.stmts[i], alloc, captures, recv_name));
        i += 1;
    }
    let tail = b.tail.as_ref().map(|e| clone_expr_fresh_ids(e, alloc, captures, recv_name));
    Block { stmts, tail, span: b.span.copy() }
}

fn clone_stmt_fresh_ids(s: &Stmt, alloc: &mut NodeIdAllocator, captures: &[(String, CaptureMode)], recv_name: &str) -> Stmt {
    match s {
        Stmt::Let(ls) => Stmt::Let(LetStmt {
            pattern: clone_pattern_fresh_ids(&ls.pattern, alloc),
            ty: ls.ty.clone(),
            value: clone_expr_fresh_ids(&ls.value, alloc, captures, recv_name),
            else_block: ls.else_block.as_ref().map(|eb| Box::new(clone_block_fresh_ids(eb, alloc, captures, recv_name))),
        }),
        Stmt::Assign(a) => Stmt::Assign(AssignStmt {
            lhs: clone_expr_fresh_ids(&a.lhs, alloc, captures, recv_name),
            rhs: clone_expr_fresh_ids(&a.rhs, alloc, captures, recv_name),
            span: a.span.copy(),
        }),
        Stmt::Expr(e) => Stmt::Expr(clone_expr_fresh_ids(e, alloc, captures, recv_name)),
        Stmt::Use(u) => Stmt::Use(u.clone()),
    }
}

fn clone_pattern_fresh_ids(p: &Pattern, alloc: &mut NodeIdAllocator) -> Pattern {
    let kind = match &p.kind {
        PatternKind::Wildcard => PatternKind::Wildcard,
        PatternKind::LitInt(n) => PatternKind::LitInt(*n),
        PatternKind::LitBool(b) => PatternKind::LitBool(*b),
        PatternKind::Binding { name, name_span, by_ref, mutable } => PatternKind::Binding {
            name: name.clone(),
            name_span: name_span.copy(),
            by_ref: *by_ref,
            mutable: *mutable,
        },
        PatternKind::VariantTuple { path, elems } => PatternKind::VariantTuple {
            path: path.clone(),
            elems: elems.iter().map(|e| clone_pattern_fresh_ids(e, alloc)).collect(),
        },
        PatternKind::VariantStruct { path, fields, rest } => PatternKind::VariantStruct {
            path: path.clone(),
            fields: fields
                .iter()
                .map(|f| FieldPattern {
                    name: f.name.clone(),
                    name_span: f.name_span.copy(),
                    pattern: clone_pattern_fresh_ids(&f.pattern, alloc),
                })
                .collect(),
            rest: *rest,
        },
        PatternKind::Tuple(elems) => PatternKind::Tuple(
            elems.iter().map(|e| clone_pattern_fresh_ids(e, alloc)).collect(),
        ),
        PatternKind::Ref { inner, mutable } => PatternKind::Ref {
            inner: Box::new(clone_pattern_fresh_ids(inner, alloc)),
            mutable: *mutable,
        },
        PatternKind::Or(alts) => PatternKind::Or(
            alts.iter().map(|a| clone_pattern_fresh_ids(a, alloc)).collect(),
        ),
        PatternKind::Range { lo, hi } => PatternKind::Range { lo: *lo, hi: *hi },
        PatternKind::At { name, name_span, inner } => PatternKind::At {
            name: name.clone(),
            name_span: name_span.copy(),
            inner: Box::new(clone_pattern_fresh_ids(inner, alloc)),
        },
    };
    Pattern {
        kind,
        span: p.span.copy(),
        id: alloc.alloc(),
    }
}

// Convert an RType (resolved type from typeck) into an AST Type so we
// can reuse it inside synthesized signatures. Only the variants that
// appear in closure param/return positions today are handled — this
// will need extension as captures land (Ref captures need lifetimes).
fn rtype_to_ast_type(rt: &RType, span: &Span, source_file: &str) -> Result<Type, Error> {
    let kind = match rt {
        RType::Int(k) => TypeKind::Path(simple_path(typeck::int_kind_name(k), span)),
        RType::Bool => TypeKind::Path(simple_path("bool", span)),
        RType::Char => TypeKind::Path(simple_path("char", span)),
        RType::Str => TypeKind::Path(simple_path("str", span)),
        RType::Tuple(elems) => {
            let mut tys: Vec<Type> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                tys.push(rtype_to_ast_type(&elems[i], span, source_file)?);
                i += 1;
            }
            TypeKind::Tuple(tys)
        }
        RType::Struct { path, type_args, .. } => {
            TypeKind::Path(struct_or_enum_path(path, type_args, span, source_file)?)
        }
        RType::Enum { path, type_args, .. } => {
            TypeKind::Path(struct_or_enum_path(path, type_args, span, source_file)?)
        }
        RType::Ref { inner, mutable, .. } => TypeKind::Ref {
            inner: Box::new(rtype_to_ast_type(inner, span, source_file)?),
            mutable: *mutable,
            lifetime: None,
        },
        RType::RawPtr { inner, mutable } => TypeKind::RawPtr {
            inner: Box::new(rtype_to_ast_type(inner, span, source_file)?),
            mutable: *mutable,
        },
        RType::Slice(inner) => TypeKind::Slice(Box::new(rtype_to_ast_type(inner, span, source_file)?)),
        RType::Param(name) => TypeKind::Path(simple_path(name, span)),
        RType::Never => TypeKind::Never,
        RType::AssocProj { .. } => {
            return Err(Error {
                file: source_file.to_string(),
                message: "internal: AssocProj in closure type — not yet handled by lowering"
                    .to_string(),
                span: span.copy(),
            });
        }
    };
    Ok(Type { kind, span: span.copy() })
}

fn simple_path(name: &str, span: &Span) -> Path {
    Path {
        segments: vec![PathSegment {
            name: name.to_string(),
            span: span.copy(),
            lifetime_args: Vec::new(),
            args: Vec::new(),
        }],
        span: span.copy(),
    }
}

fn struct_or_enum_path(
    path: &Vec<String>,
    type_args: &Vec<RType>,
    span: &Span,
    source_file: &str,
) -> Result<Path, Error> {
    let mut segments: Vec<PathSegment> = path
        .iter()
        .map(|s| PathSegment {
            name: s.clone(),
            span: span.copy(),
            lifetime_args: Vec::new(),
            args: Vec::new(),
        })
        .collect();
    if !type_args.is_empty() && !segments.is_empty() {
        let last = segments.len() - 1;
        let mut tys: Vec<Type> = Vec::new();
        let mut i = 0;
        while i < type_args.len() {
            tys.push(rtype_to_ast_type(&type_args[i], span, source_file)?);
            i += 1;
        }
        segments[last].args = tys;
    }
    Ok(Path { segments, span: span.copy() })
}

// Register a synthesized impl in TraitTable + FuncTable + run typeck on
// its method body. The impl's target is always a unit struct that's
// already in StructTable (registered at end of typeck::check). We
// allocate a fresh wasm idx for the method, populate FnSymbol entries
// with concrete param/return types pulled from the impl AST, then
// invoke `check_function` to fill in the body's typing artifacts.
fn register_synthesized_impl(
    ib: &ImplBlock,
    parent_module_path: &Vec<String>,
    source_file: &str,
    structs: &mut StructTable,
    enums: &mut typeck::EnumTable,
    aliases: &mut typeck::AliasTable,
    traits: &mut TraitTable,
    funcs: &mut FuncTable,
    reexports: &mut ReExportTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    typeck::register_synthesized_closure_impl(
        ib,
        parent_module_path,
        source_file,
        structs,
        enums,
        aliases,
        traits,
        funcs,
        reexports,
        next_idx,
    )
}
