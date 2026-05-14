use crate::ast::{
    Block, Expr, ExprKind, Function, Item, LetStmt, MethodCall, Module, NodeId, Pattern,
    PatternKind, Stmt,
};
use crate::span::{Error, Span};
use crate::typeck::{
    CallResolution, EnumTable, FuncTable, LifetimeRepr, MethodCandidate, MethodResolution,
    MovedPlace, RType, StructTable, TraitTable, drop_trait_path, find_inherent_synth_idx,
    find_trait_impl_idx_by_span, find_trait_impl_method, func_lookup, is_drop, peel_opaque,
    rtype_eq, solve_impl, solve_impl_with_args, subst_and_peel, substitute_rtype,
};

// One fully-substituted monomorphization ready for codegen. Either a
// non-generic function (env empty, artifacts already concrete) or a
// (template, concrete type_args) instantiation (env populated, artifacts
// substituted through env).
//
// Lifetime `'a` borrows the source AST: for non-generic functions, from
// `MonoFnInput`: what `lower_to_mono` consumes — AST body + typeck-
// computed artifacts (per-NodeId types and resolutions, borrowck's
// move-state snapshot, etc.). Holds a `&'a Function` borrow into
// either the user/library `Module` or `FuncTable.templates[idx].func`;
// both outlive the lowering call. Built by `emit_function` /
// `emit_monomorphic` and discarded immediately after lowering — only
// `MonoFn` (which owns the lowered body) flows into codegen.
pub struct MonoFnInput<'a> {
    pub func: &'a Function,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub expr_types: Vec<Option<RType>>,
    pub method_resolutions: Vec<Option<MethodResolution>>,
    pub call_resolutions: Vec<Option<CallResolution>>,
    pub builtin_type_targets: Vec<Option<Vec<RType>>>,
    pub moved_places: Vec<MovedPlace>,
    pub move_sites: Vec<(NodeId, String)>,
    // Per-pattern.id ergonomics record from typeck (auto-peel layer
    // count + per-Binding mode override). Mono lowering reads this to
    // produce a desugared AST pattern with explicit `&` wrappers and
    // `ref` bindings — codegen sees only explicit-form patterns.
    pub pattern_ergo: Vec<crate::typeck::PatternErgo>,
    // Per-NodeId bare-closure-call records — see FnSymbol.
    // `bare_closure_calls`. mono consults this when lowering
    // ExprKind::Call to detect closure-bare-calls and rewrite as a
    // MethodCall MonoExpr.
    pub bare_closure_calls: Vec<Option<String>>,
    // Per-NodeId resolved const value (see `FnSymbol.const_uses`).
    // At each `Var` lowering site, mono checks this table; if Some,
    // the Var is a const reference and gets lowered to a `Lit`
    // MonoExpr carrying the value.
    pub const_uses: Vec<Option<crate::typeck::ConstValue>>,
    // Per-NodeId fn-item address (see `FnSymbol.fn_item_addrs`). At
    // each `Var` lowering site, mono checks this table; if Some, the
    // Var is a fn-item-as-fn-pointer and lowers to `MonoExprKind::
    // FnItemAddr` carrying the resolved wasm idx.
    pub fn_item_addrs: Vec<Option<usize>>,
    // Per-NodeId dyn-trait coercion (see `FnSymbol.dyn_coercions`).
    // When set, the matching expression is wrapped in
    // `MonoExprKind::RefDynCoerce` after lowering its inner ref.
    pub dyn_coercions: Vec<Option<crate::typeck::DynCoercion>>,
    // Per-NodeId dyn-method dispatch (see `FnSymbol.dyn_method_calls`).
    // When set on a MethodCall expr, the call lowers to
    // `MonoExprKind::DynMethodCall` instead of going through the
    // standard impl-resolution path.
    pub dyn_method_calls: Vec<Option<crate::typeck::DynMethodDispatch>>,
    pub wasm_idx: u32,
    pub is_export: bool,
}

// `MonoFn`: what `emit_function_concrete` consumes — owns the lowered
// `MonoBody` plus the codegen-time scaffolding (signature, drop-state
// snapshot, wasm idx). No AST reference, no typeck input caches —
// those were consumed by lowering and don't outlive it.
pub struct MonoFn {
    pub name: String,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub body: MonoBody,
    pub moved_places: Vec<MovedPlace>,
    pub move_sites: Vec<(NodeId, String)>,
    pub wasm_idx: u32,
    pub is_export: bool,
}

// Intern table mapping `(template_idx, concrete type_args)` pairs to
// pre-allocated wasm function indices. Lives across the whole codegen
// pipeline for one crate — populated by `expand` (eagerly, before
// codegen runs) and consulted by codegen at every dispatch site that
// would otherwise have allocated a new index.
//
// `entries` grows monotonically. Walkers (expand, codegen byte
// emission) use an index cursor to handle "more entries may appear
// during the walk" — index-based iteration naturally covers entries
// added mid-loop.
pub struct MonoTable {
    entries: Vec<(usize, Vec<RType>, u32)>,
    next_idx: u32,
}

impl MonoTable {
    pub fn new(start_idx: u32) -> Self {
        Self {
            entries: Vec::new(),
            next_idx: start_idx,
        }
    }

    pub fn next_idx(&self) -> u32 {
        self.next_idx
    }

    // Reserve a wasm idx for a synthesized non-mono function (e.g.
    // codegen's no-op drop). Bumps `next_idx` past the reservation
    // so subsequent monomorphizations don't collide.
    pub fn reserve_idx(&mut self) -> u32 {
        let i = self.next_idx;
        self.next_idx += 1;
        i
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entry(&self, i: usize) -> (usize, &Vec<RType>, u32) {
        let e = &self.entries[i];
        (e.0, &e.1, e.2)
    }

    pub fn lookup(&self, template_idx: usize, args: &Vec<RType>) -> Option<u32> {
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].0 == template_idx
                && rtype_vec_eq(&self.entries[i].1, args)
            {
                return Some(self.entries[i].2);
            }
            i += 1;
        }
        None
    }

    // Allocates a fresh wasm idx if `(template_idx, args)` isn't already
    // interned, returns the idx. Idempotent across calls with the same key.
    pub fn intern(&mut self, template_idx: usize, args: Vec<RType>) -> u32 {
        if let Some(idx) = self.lookup(template_idx, &args) {
            return idx;
        }
        let idx = self.next_idx;
        self.next_idx += 1;
        self.entries.push((template_idx, args, idx));
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

fn build_env(type_params: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < type_params.len() {
        env.push((type_params[i].clone(), type_args[i].clone()));
        i += 1;
    }
    env
}

fn subst_vec(v: &Vec<RType>, env: &Vec<(String, RType)>, funcs: &FuncTable) -> Vec<RType> {
    let mut out: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(subst_and_peel(&v[i], env, funcs));
        i += 1;
    }
    out
}

// Walk a function body and discover every (template, args) mono it
// requires, registering each via `table.intern`. Doesn't emit bytes.
// Mirrors codegen's body traversal for the dispatch sites that would
// otherwise call `mono.intern` lazily.
fn discover_in_body(
    func: &Function,
    expr_types: &Vec<Option<RType>>,
    method_resolutions: &Vec<Option<MethodResolution>>,
    call_resolutions: &Vec<Option<CallResolution>>,
    param_types: &Vec<RType>,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    // Drop monos for parameters: any param whose substituted type is
    // Drop will trigger a Drop::drop call at function scope-end.
    let mut p = 0;
    while p < param_types.len() {
        let pty = subst_and_peel(&param_types[p], env, funcs);
        register_drop_mono(&pty, traits, funcs, table);
        p += 1;
    }
    walk_block(
        &func.body,
        expr_types,
        method_resolutions,
        call_resolutions,
        env,
        structs,
        enums,
        traits,
        funcs,
        table,
    );
}

fn walk_block(
    block: &Block,
    expr_types: &Vec<Option<RType>>,
    method_resolutions: &Vec<Option<MethodResolution>>,
    call_resolutions: &Vec<Option<CallResolution>>,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(ls) => walk_let(
                ls,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            ),
            Stmt::Assign(a) => {
                walk_expr(
                    &a.lhs,
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                walk_expr(
                    &a.rhs,
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
            }
            Stmt::Expr(e) => walk_expr(
                e,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            ),
            Stmt::Use(_) => {}
        }
        i += 1;
    }
    if let Some(tail) = &block.tail {
        walk_expr(
            tail,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        );
    }
}

fn walk_let(
    ls: &LetStmt,
    expr_types: &Vec<Option<RType>>,
    method_resolutions: &Vec<Option<MethodResolution>>,
    call_resolutions: &Vec<Option<CallResolution>>,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    if let Some(value) = &ls.value {
        walk_expr(
            value,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        );
    }
    // Walk the let-else divergent block too.
    if let Some(else_block) = &ls.else_block {
        walk_block(
            else_block,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        );
    }
    // The let's value type may be Drop — register Drop::drop for it.
    // For uninit `let x: T;`, the binding's type is recorded on the
    // pattern instead (no value expr exists).
    let value_ty: Option<&RType> = ls
        .value
        .as_ref()
        .and_then(|v| expr_types[v.id as usize].as_ref());
    if let Some(ty) = value_ty {
        let concrete = subst_and_peel(ty, env, funcs);
        register_drop_mono(&concrete, traits, funcs, table);
        // Walk pattern bindings to register Drop monos for any
        // destructured Drop-typed leaves.
        register_drop_for_pattern_bindings(
            &ls.pattern,
            &concrete,
            structs,
            enums,
            traits,
            funcs,
            table,
        );
    }
}

// Walk a pattern that binds against `scrut_ty`, recursively descending
// into struct/tuple/variant patterns and registering Drop monos for any
// binding leaves whose type is Drop. The leaf-type derivation mirrors
// what codegen does when it computes pattern binding types.
fn register_drop_for_pattern_bindings(
    pattern: &Pattern,
    scrut_ty: &RType,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    match &pattern.kind {
        PatternKind::Binding { .. } => {
            register_drop_mono(scrut_ty, traits, funcs, table);
        }
        PatternKind::At { inner, .. } => {
            register_drop_mono(scrut_ty, traits, funcs, table);
            register_drop_for_pattern_bindings(
                inner, scrut_ty, structs, enums, traits, funcs, table,
            );
        }
        // Other pattern shapes (literal, wildcard, struct/tuple/variant
        // destructuring, or-pattern, range) — we conservatively skip the
        // detailed type derivation here. Drop monos for destructured
        // leaves are registered when their bound name shows up as a
        // typed binding via codegen's locals walk; we cover the explicit
        // let case above via expr_types.
        _ => {}
    }
}

// Register the Drop::drop monomorphization for `ty` if `ty` is Drop and
// the impl's drop method is a template. For non-Drop types this is a
// no-op; for Drop with a non-template (Direct) impl this is also a
// no-op since there's nothing to monomorphize.
fn register_drop_mono(ty: &RType, traits: &TraitTable, funcs: &FuncTable, table: &mut MonoTable) {
    // Box<dyn Trait> uses a vtable-driven drop emitted directly by
    // codegen (see `emit_drop_walker`'s Box-dyn short-circuit). The
    // user-written `impl<T> Drop for Box<T>` body assumes sized T, so
    // monomorphizing it for T = dyn would fail. Skip.
    if let RType::Struct { path, type_args, .. } = ty {
        if crate::typeck::is_std_box_path(path)
            && type_args.len() == 1
            && matches!(&type_args[0], RType::Dyn { .. })
        {
            return;
        }
    }
    if !is_drop(ty, traits) {
        return;
    }
    let drop_path = drop_trait_path();
    let resolution = match solve_impl(&drop_path, ty, traits, 0) {
        Some(r) => r,
        None => return, // is_drop true but no impl found — shouldn't happen, but skip
    };
    let cand = match find_trait_impl_method(funcs, resolution.impl_idx, "drop") {
        Some(c) => c,
        None => return,
    };
    if let MethodCandidate::Template(i) = cand {
        let tmpl = &funcs.templates[i];
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
            concrete.push(found.expect("impl-param bound by subst"));
            k += 1;
        }
        table.intern(i, concrete);
    }
}

fn walk_expr(
    expr: &Expr,
    expr_types: &Vec<Option<RType>>,
    method_resolutions: &Vec<Option<MethodResolution>>,
    call_resolutions: &Vec<Option<CallResolution>>,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let id = expr.id as usize;
    match &expr.kind {
        ExprKind::IntLit(_)
        | ExprKind::NegIntLit(_)
        | ExprKind::StrLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Var(_)
        | ExprKind::Break { .. }
        | ExprKind::Continue { .. } => {}
        ExprKind::If(if_expr) => {
            walk_expr(
                &if_expr.cond,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_block(
                if_expr.then_block.as_ref(),
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_block(
                if_expr.else_block.as_ref(),
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
        }
        ExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr(
                    &args[i],
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                i += 1;
            }
        }
        ExprKind::Borrow { inner, .. }
        | ExprKind::Cast { inner, .. }
        | ExprKind::Try { inner, .. } => walk_expr(
            inner,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        ),
        ExprKind::Deref(inner) => {
            walk_expr(
                inner,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            // Smart-pointer deref: if inner's type is a struct (not Ref/RawPtr),
            // codegen calls Deref::deref. Register that mono here.
            if let Some(inner_ty) = &expr_types[inner.id as usize] {
                let inner_ty = subst_and_peel(inner_ty, env, funcs);
                if !matches!(&inner_ty, RType::Ref { .. } | RType::RawPtr { .. }) {
                    register_deref_mono(&inner_ty, "Deref", "deref", traits, funcs, table);
                }
            }
        }
        ExprKind::FieldAccess(fa) => walk_expr(
            &fa.base,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        ),
        ExprKind::TupleIndex { base, .. } => walk_expr(
            base,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        ),
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                walk_expr(
                    &c.args[i],
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                i += 1;
            }
            // CallResolution::Generic → register the (template, args) mono.
            if let Some(CallResolution::Generic { template_idx, type_args }) =
                &call_resolutions[id]
            {
                let concrete = subst_vec(type_args, env, funcs);
                table.intern(*template_idx, concrete);
            }
        }
        ExprKind::MethodCall(mc) => walk_method_call(
            mc,
            id,
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        ),
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                walk_expr(
                    &s.fields[i].value,
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                i += 1;
            }
        }
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                walk_expr(
                    &elems[i],
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                i += 1;
            }
        }
        ExprKind::Block(b) | ExprKind::Unsafe(b) => walk_block(
            b.as_ref(),
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        ),
        ExprKind::Match(m) => {
            walk_expr(
                &m.scrutinee,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            let mut i = 0;
            while i < m.arms.len() {
                walk_expr(
                    &m.arms[i].body,
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                i += 1;
            }
        }
        ExprKind::IfLet(il) => {
            walk_expr(
                &il.scrutinee,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_block(
                il.then_block.as_ref(),
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_block(
                il.else_block.as_ref(),
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
        }
        ExprKind::While(w) => {
            walk_expr(
                &w.cond,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_block(
                w.body.as_ref(),
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
        }
        ExprKind::For(f) => {
            walk_expr(
                &f.iter,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_block(
                f.body.as_ref(),
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            // for-in lowers to Iterator::next(&mut iter). Register its mono.
            if let Some(iter_ty) = &expr_types[f.iter.id as usize] {
                let iter_ty = subst_and_peel(iter_ty, env, funcs);
                let iterator_path = vec![
                    "std".to_string(),
                    "iter".to_string(),
                    "Iterator".to_string(),
                ];
                register_trait_method_mono_via_solve_impl(
                    &iterator_path,
                    &iter_ty,
                    "next",
                    traits,
                    funcs,
                    table,
                );
            }
        }
        ExprKind::Return { value } => {
            if let Some(v) = value {
                walk_expr(
                    v,
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
            }
        }
        ExprKind::Index { base, index, .. } => {
            walk_expr(
                base,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            walk_expr(
                index,
                expr_types,
                method_resolutions,
                call_resolutions,
                env,
                structs,
                enums,
                traits,
                funcs,
                table,
            );
            // arr[i] / arr[range] dispatches to Index::index; mutable
            // contexts use IndexMut::index_mut — but we can't tell from
            // the Expr node alone whether the codegen will choose
            // mut. Conservatively register both Index and IndexMut
            // monos. The unused one is harmless dead code.
            if let (Some(base_ty), Some(idx_ty)) =
                (&expr_types[base.id as usize], &expr_types[index.id as usize])
            {
                let base_ty = subst_and_peel(base_ty, env, funcs);
                let idx_ty = subst_and_peel(idx_ty, env, funcs);
                let lookup_rt = match &base_ty {
                    RType::Ref { inner, .. } => (**inner).clone(),
                    _ => base_ty.clone(),
                };
                let trait_index = vec![
                    "std".to_string(),
                    "ops".to_string(),
                    "Index".to_string(),
                ];
                let trait_index_mut = vec![
                    "std".to_string(),
                    "ops".to_string(),
                    "IndexMut".to_string(),
                ];
                register_trait_method_mono_via_solve_with_args(
                    &trait_index,
                    &vec![idx_ty.clone()],
                    &lookup_rt,
                    "index",
                    traits,
                    funcs,
                    table,
                );
                register_trait_method_mono_via_solve_with_args(
                    &trait_index_mut,
                    &vec![idx_ty],
                    &lookup_rt,
                    "index_mut",
                    traits,
                    funcs,
                    table,
                );
            }
        }
        ExprKind::MacroCall { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr(
                    &args[i],
                    expr_types,
                    method_resolutions,
                    call_resolutions,
                    env,
                    structs,
                    enums,
                    traits,
                    funcs,
                    table,
                );
                i += 1;
            }
        }
        ExprKind::Closure(_) => {
            unreachable!("closure expressions rejected at typeck before mono")
        }
    }
}

fn walk_method_call(
    mc: &MethodCall,
    id: usize,
    expr_types: &Vec<Option<RType>>,
    method_resolutions: &Vec<Option<MethodResolution>>,
    call_resolutions: &Vec<Option<CallResolution>>,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    walk_expr(
        &mc.receiver,
        expr_types,
        method_resolutions,
        call_resolutions,
        env,
        structs,
        enums,
        traits,
        funcs,
        table,
    );
    let mut i = 0;
    while i < mc.args.len() {
        walk_expr(
            &mc.args[i],
            expr_types,
            method_resolutions,
            call_resolutions,
            env,
            structs,
            enums,
            traits,
            funcs,
            table,
        );
        i += 1;
    }
    let mr = match &method_resolutions[id] {
        Some(m) => m,
        None => return,
    };
    if let Some(td) = &mr.trait_dispatch {
        // Trait-dispatched method call. Substitute recv and trait_args
        // through env, solve the impl, find the method, register the
        // mono if it's a Template.
        let concrete_recv = subst_and_peel(&td.recv_type, env, funcs);
        let concrete_recv_for_solve = match &concrete_recv {
            RType::Ref { inner, .. } => (**inner).clone(),
            other => other.clone(),
        };
        let concrete_trait_args = subst_vec(&td.trait_args, env, funcs);
        let resolution = match solve_impl_with_args(
            &td.trait_path,
            &concrete_trait_args,
            &concrete_recv_for_solve,
            traits,
            0,
        ) {
            Some(r) => r,
            None => return,
        };
        let cand = match find_trait_impl_method(funcs, resolution.impl_idx, &td.method_name) {
            Some(c) => c,
            None => return,
        };
        if let MethodCandidate::Template(i) = cand {
            let tmpl = &funcs.templates[i];
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
                concrete.push(found.expect("impl-param bound by subst"));
                k += 1;
            }
            let method_param_count = tmpl.type_params.len() - impl_param_count;
            let recorded_type_args = mr.type_args.clone();
            if recorded_type_args.len() == method_param_count {
                let mut k = 0;
                while k < method_param_count {
                    concrete.push(subst_and_peel(&recorded_type_args[k], env, funcs));
                    k += 1;
                }
                table.intern(i, concrete);
            }
        }
        return;
    }
    // Non-trait dispatched: direct template_idx + type_args.
    if let Some(template_idx) = mr.template_idx {
        let concrete = subst_vec(&mr.type_args, env, funcs);
        table.intern(template_idx, concrete);
    }
}

// Solve an impl by recv-type only (no trait args), find the named
// method, and register the Template mono if applicable.
fn register_trait_method_mono_via_solve_impl(
    trait_path: &Vec<String>,
    recv_ty: &RType,
    method_name: &str,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let resolution = match solve_impl(trait_path, recv_ty, traits, 0) {
        Some(r) => r,
        None => return,
    };
    let cand = match find_trait_impl_method(funcs, resolution.impl_idx, method_name) {
        Some(c) => c,
        None => return,
    };
    if let MethodCandidate::Template(i) = cand {
        let tmpl = &funcs.templates[i];
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
            concrete.push(found.expect("impl-param bound by subst"));
            k += 1;
        }
        table.intern(i, concrete);
    }
}

// Solve an impl by recv-type + trait args (for generic-trait impls
// like `Index<Idx>`), find the named method, register Template mono.
fn register_trait_method_mono_via_solve_with_args(
    trait_path: &Vec<String>,
    trait_args: &Vec<RType>,
    recv_ty: &RType,
    method_name: &str,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let resolution = match solve_impl_with_args(trait_path, trait_args, recv_ty, traits, 0) {
        Some(r) => r,
        None => return,
    };
    let cand = match find_trait_impl_method(funcs, resolution.impl_idx, method_name) {
        Some(c) => c,
        None => return,
    };
    if let MethodCandidate::Template(i) = cand {
        let tmpl = &funcs.templates[i];
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
            concrete.push(found.expect("impl-param bound by subst"));
            k += 1;
        }
        table.intern(i, concrete);
    }
}

// Same shape as solve_impl-based, used for Deref/DerefMut.
fn register_deref_mono(
    inner_ty: &RType,
    trait_name: &str,
    method_name: &str,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let trait_path = vec![
        "std".to_string(),
        "ops".to_string(),
        trait_name.to_string(),
    ];
    register_trait_method_mono_via_solve_impl(
        &trait_path, inner_ty, method_name, traits, funcs, table,
    );
}

// Top-level expansion entry point. Walks every non-generic function in
// `module` (recursively into nested modules), discovers monos via
// `discover_in_body`, and drains the queue until no new monos appear.
// On return, `table` contains every (template, args) → wasm_idx that
// codegen will need.
pub fn expand(
    module: &Module,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) -> Result<(), Error> {
    let mut path: Vec<String> = Vec::new();
    if !module.name.is_empty() {
        path.push(module.name.clone());
    }
    walk_module(module, &mut path, structs, enums, traits, funcs, table);
    // Walk newly-discovered template instances' bodies. Index-based
    // iteration handles "more may appear mid-walk."
    let mut i = 0;
    while i < table.len() {
        let (template_idx, args_ref, _idx) = table.entry(i);
        let type_args = args_ref.clone();
        let tmpl = &funcs.templates[template_idx];
        let env = build_env(&tmpl.type_params, &type_args);
        // Snapshot the template's artifact references; discover_in_body
        // doesn't keep references past its own call.
        discover_in_body(
            &tmpl.func,
            &tmpl.expr_types,
            &tmpl.method_resolutions,
            &tmpl.call_resolutions,
            &tmpl.param_types,
            &env,
            structs,
            enums,
            traits,
            funcs,
            table,
        );
        i += 1;
    }
    Ok(())
}

fn walk_module(
    module: &Module,
    path: &mut Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let empty_env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                if f.type_params.is_empty() {
                    walk_non_generic(f, path, &empty_env, structs, enums, traits, funcs, table);
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                walk_module(m, path, structs, enums, traits, funcs, table);
                path.pop();
            }
            Item::Impl(ib) => {
                let method_prefix =
                    compute_impl_method_prefix(ib, path, &module.source_file, traits, funcs);
                let impl_is_generic = !ib.type_params.is_empty();
                let mut k = 0;
                while k < ib.methods.len() {
                    let method_is_generic =
                        impl_is_generic || !ib.methods[k].type_params.is_empty();
                    if !method_is_generic {
                        walk_non_generic(
                            &ib.methods[k],
                            &method_prefix,
                            &empty_env,
                            structs,
                            enums,
                            traits,
                            funcs,
                            table,
                        );
                    }
                    k += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

// Compute the path prefix that typeck stored the impl's methods under.
// Mirrors codegen::emit_module's logic so func_lookup finds the right
// FnSymbol.
fn compute_impl_method_prefix(
    ib: &crate::ast::ImplBlock,
    path: &Vec<String>,
    source_file: &str,
    traits: &TraitTable,
    funcs: &FuncTable,
) -> Vec<String> {
    let target_name = match &ib.target.kind {
        crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
            Some(p.segments[0].name.clone())
        }
        _ => None,
    };
    let trait_impl_idx = if ib.trait_path.is_some() {
        find_trait_impl_idx_by_span(traits, source_file, &ib.span)
    } else {
        None
    };
    let trait_is_generic = trait_impl_idx.map_or(false, |idx| {
        !traits.impls[idx].trait_args.is_empty()
    });
    let mut method_prefix = path.clone();
    match target_name {
        Some(name) => {
            method_prefix.push(name);
            if trait_is_generic {
                if let Some(idx) = trait_impl_idx {
                    method_prefix.push(format!("__trait_impl_{}", idx));
                }
            }
        }
        None => {
            if let Some(idx) = trait_impl_idx {
                method_prefix.push(format!("__trait_impl_{}", idx));
            } else if let Some(idx) = find_inherent_synth_idx(funcs, source_file, &ib.span) {
                method_prefix.push(format!("__inherent_synth_{}", idx));
            }
        }
    }
    let _ = LifetimeRepr::Named(String::new()); // keep import live
    method_prefix
}

fn walk_non_generic(
    func: &Function,
    path_prefix: &Vec<String>,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    table: &mut MonoTable,
) {
    let mut full = path_prefix.clone();
    full.push(func.name.clone());
    let entry = match func_lookup(funcs, &full) {
        Some(e) => e,
        None => return, // typeck didn't register this function (shouldn't happen)
    };
    discover_in_body(
        func,
        &entry.expr_types,
        &entry.method_resolutions,
        &entry.call_resolutions,
        &entry.param_types,
        env,
        structs,
        enums,
        traits,
        funcs,
        table,
    );
}

// ============================================================================
// Mono IR — post-mono, fully-substituted intermediate representation.
//
// Codegen will eventually consume this instead of walking the typed AST + per-
// `Expr.id` resolution side tables. Every node carries its concrete `RType`
// (no `Param`), every dispatch carries its resolved `wasm_idx`, and Drop
// becomes a first-class statement so codegen has no implicit scope-end logic.
//
// Phase 1a (this commit): types defined + skeleton lowering function. Not
// yet wired to codegen; lowering's output is computed and discarded so the
// types are exercised without disturbing existing behavior.
// ============================================================================

use crate::ast::Pattern as AstPattern;

// Rebuild an AST pattern with explicit `&` wrappers and `ref` bindings
// per typeck's match-ergonomics decisions (see `PatternErgo`). The
// original AST is untouched — this returns a freshly constructed
// pattern tree that codegen can dispatch on without needing to know
// about ergonomics. Wrapper Ref nodes get fresh-but-deterministic IDs
// (the original pattern's id + an offset that is never queried by
// codegen for non-Binding patterns), so Binding-pattern IDs are
// preserved and existing pattern_id-keyed lookups continue to work.
fn desugar_pattern(
    pattern: &AstPattern,
    pattern_ergo: &Vec<crate::typeck::PatternErgo>,
) -> AstPattern {
    use crate::ast::PatternKind;
    let pid = pattern.id as usize;
    let ergo = if pid < pattern_ergo.len() {
        pattern_ergo[pid]
    } else {
        crate::typeck::PatternErgo::default()
    };
    // First, recursively desugar children. Then apply this node's
    // binding override (for Binding/At) or peel wrap.
    let inner_kind = match &pattern.kind {
        PatternKind::Wildcard
        | PatternKind::LitInt(_)
        | PatternKind::LitBool(_)
        | PatternKind::Range { .. } => pattern.kind.clone(),
        PatternKind::Binding { name, name_span, by_ref, mutable } => {
            let (eff_by_ref, eff_mutable) = if ergo.binding_override_ref {
                (true, ergo.binding_mutable_ref)
            } else {
                (*by_ref, *mutable)
            };
            PatternKind::Binding {
                name: name.clone(),
                name_span: name_span.copy(),
                by_ref: eff_by_ref,
                mutable: eff_mutable,
            }
        }
        PatternKind::At { name, name_span, inner } => PatternKind::At {
            name: name.clone(),
            name_span: name_span.copy(),
            inner: Box::new(desugar_pattern(inner, pattern_ergo)),
        },
        PatternKind::VariantTuple { path, elems } => {
            let new_elems = elems
                .iter()
                .map(|e| desugar_pattern(e, pattern_ergo))
                .collect();
            PatternKind::VariantTuple { path: path.clone(), elems: new_elems }
        }
        PatternKind::VariantStruct { path, fields, rest } => {
            let new_fields = fields
                .iter()
                .map(|fp| crate::ast::FieldPattern {
                    name: fp.name.clone(),
                    name_span: fp.name_span.copy(),
                    pattern: desugar_pattern(&fp.pattern, pattern_ergo),
                })
                .collect();
            PatternKind::VariantStruct {
                path: path.clone(),
                fields: new_fields,
                rest: *rest,
            }
        }
        PatternKind::Tuple(elems) => {
            let new_elems = elems
                .iter()
                .map(|e| desugar_pattern(e, pattern_ergo))
                .collect();
            PatternKind::Tuple(new_elems)
        }
        PatternKind::Ref { inner, mutable } => PatternKind::Ref {
            inner: Box::new(desugar_pattern(inner, pattern_ergo)),
            mutable: *mutable,
        },
        PatternKind::Or(alts) => {
            let new_alts = alts
                .iter()
                .map(|a| desugar_pattern(a, pattern_ergo))
                .collect();
            PatternKind::Or(new_alts)
        }
    };
    let mut current = AstPattern {
        kind: inner_kind,
        span: pattern.span.copy(),
        id: pattern.id,
    };
    // Wrap in N explicit Ref layers (outermost peel = outermost wrap).
    // Bit i of `peel_mut_bits` (set during peel) tells us whether the
    // i-th outermost peel was `&mut`. Wrapping reverses: layer 0 (the
    // outermost) is constructed last so it ends up wrapping everything.
    let mut layer = ergo.peel_layers;
    while layer > 0 {
        layer -= 1;
        let mutable = (ergo.peel_mut_bits >> layer) & 1 != 0;
        current = AstPattern {
            kind: PatternKind::Ref {
                inner: Box::new(current),
                mutable,
            },
            span: pattern.span.copy(),
            id: pattern.id, // codegen doesn't query Ref-pattern IDs
        };
    }
    current
}

// Per-function unique identifier for a binding (param, let, or pattern leaf).
// Indexes into `MonoBody.locals`. Allocated by the lowering pass in
// declaration order: params first (0..N), then lets/pattern bindings as
// they're encountered in source order.
#[allow(dead_code)]
pub type BindingId = u32;

#[allow(dead_code)]
pub struct MonoLocal {
    pub id: BindingId,
    pub name: String,
    pub ty: RType,
    // Origin: which AST node (or synthesizer) this binding comes from.
    // Storage and drop-action decisions live in a separate post-
    // lowering layout that walks the Mono IR.
    pub origin: BindingOrigin,
}

#[allow(dead_code)]
#[derive(Clone)]
pub enum BindingOrigin {
    Param(usize),       // index into MonoFn.param_types
    LetValue,           // bound by a `Stmt::Let` (no further info needed —
                        // codegen consults `MonoLayout.binding_storage[binding_id]`)
    Pattern(NodeId),    // pattern Binding/At node id (kept for
                        // map_arm_pattern_bindings to wire up arm bindings)
    // Compiler-synthesized binding (`__iter` for For desugar, etc.).
    // Carries a description for diagnostics.
    Synthesized(String),
}

// Literals.
#[allow(dead_code)]
pub enum MonoLit {
    // u64 magnitude + signedness flag captured in the binding's RType.
    // For `NegIntLit(n)` lowered here, magnitude is `n` and the
    // negation is folded in during codegen (i64 cast then negate).
    Int { magnitude: u64, negated: bool },
    Bool(bool),
    Char(u32),
    Str(String),
}

// Place expressions — addressable lvalues. Used as the LHS of Assign and
// as the inner of Borrow / Deref-of-place patterns.
#[allow(dead_code)]
pub struct MonoPlace {
    pub kind: MonoPlaceKind,
    pub ty: RType,
    pub span: Span,
}

#[allow(dead_code)]
pub enum MonoPlaceKind {
    Local(BindingId),
    // `place.field` — the field's byte offset within `place`'s struct
    // type, precomputed by lowering (no need for codegen to look up
    // struct table at emission time).
    Field {
        base: Box<MonoPlace>,
        field_name: String,
        byte_offset: u32,
    },
    // `place.0` (tuple-index access).
    TupleIndex {
        base: Box<MonoPlace>,
        index: u32,
        byte_offset: u32,
    },
    // `*expr` where `expr` is a place-form expression. Codegen reads
    // the address from `inner` and treats it as an lvalue.
    Deref {
        inner: Box<MonoExpr>,
    },
}

#[allow(dead_code)]
pub struct MonoExpr {
    pub kind: MonoExprKind,
    pub ty: RType,
    pub span: Span,
}

#[allow(dead_code)]
pub enum MonoExprKind {
    Lit(MonoLit),
    // Read a binding's value. `src_node_id` is the AST node id of the
    // `Var(name)` expression that produced this read (for a synthesized
    // Local — for-loop's iter borrow, try-op's arm bodies, etc. — set
    // to u32::MAX, which doesn't appear as a real node id). Codegen
    // uses src_node_id to look up move sites: if borrowck recorded a
    // whole-binding move at this read, codegen clears the binding's
    // drop flag (`MaybeMoved` semantics).
    Local(BindingId, NodeId),
    // Read from a place (loads the value at the place's address into
    // wasm scalars / on-stack representation).
    PlaceLoad(MonoPlace),
    Borrow {
        place: MonoPlace,
        mutable: bool,
    },
    // `&value-expr` where the inner isn't a place (literals, calls,
    // etc.). Codegen materializes the value into a fresh shadow-stack
    // slot and yields its address. Lowering wraps non-place borrows
    // in this variant rather than synthesizing a Let — keeps the IR
    // shape direct.
    BorrowOfValue {
        value: Box<MonoExpr>,
        mutable: bool,
    },
    // Pre-resolved direct call. `wasm_idx` was determined at lowering
    // (from `mono::expand`'s mono table for templates, or directly
    // from `FnSymbol.idx` for non-generics).
    Call {
        wasm_idx: u32,
        args: Vec<MonoExpr>,
    },
    // Take the address of a fn item — yields an i32 funcref-table slot.
    // `wasm_idx` is the function's wasm index; codegen calls
    // `intern_table_slot(wasm_idx)` to materialize the slot index, then
    // emits `i32.const <slot>`.
    FnItemAddr {
        wasm_idx: u32,
    },
    // Indirect call through an FnPtr value. `callee` lowers to the
    // value (an i32 table slot). `fn_ptr_ty` carries the signature so
    // codegen can intern the matching FuncType in `wasm.types` and
    // pass the typeidx to `call_indirect`.
    CallIndirect {
        callee: Box<MonoExpr>,
        args: Vec<MonoExpr>,
        fn_ptr_ty: RType,
    },
    // Coerce `&T` / `&mut T` / `Box<T>` into the matching `dyn`
    // shape. `inner_ref` is the source ref/box expression (lowered to
    // its data ptr); codegen materializes the matching vtable in the
    // data segment and emits the vtable address as the second word.
    // `bounds` carries all trait paths + trait_args + assoc_bindings
    // (one entry per principal in a `dyn A + B` type); `intern_vtable`
    // builds a concatenated vtable for all bounds in declaration order
    // (post the shared drop header).
    RefDynCoerce {
        inner_ref: Box<MonoExpr>,
        src_concrete_ty: RType,
        bounds: Vec<crate::typeck::DynBound>,
    },
    // Method dispatch through a `&dyn Trait` / `&mut dyn Trait` fat
    // ref. `recv` is the receiver fat ref (data ptr + vtable ptr);
    // codegen emits args, then `recv.0` as the &self/&mut self arg,
    // then loads the function pointer at `recv.1[method_idx*4]`, then
    // `call_indirect typeidx`. typeidx comes from interning the
    // method's param/ret types.
    DynMethodCall {
        recv: Box<MonoExpr>,
        method_idx: u32,
        args: Vec<MonoExpr>,
        method_param_types: Vec<RType>,
        method_return_type: RType,
        recv_mut: bool,
        trait_path: Vec<String>,
    },
    // Pre-resolved method dispatch. `recv_adjust` says whether to
    // emit `&recv` / `&mut recv` / `recv` / pass-through.
    MethodCall {
        wasm_idx: u32,
        recv_adjust: ReceiverAdjust,
        recv: Box<MonoExpr>,
        args: Vec<MonoExpr>,
    },
    Builtin {
        name: String,
        type_args: Vec<RType>,
        args: Vec<MonoExpr>,
    },
    StructLit {
        // Resolved struct path + concrete type-args (for size calc).
        path: Vec<String>,
        type_args: Vec<RType>,
        // Field initializers, in declared order (lowering reorders if
        // user wrote them out of order).
        fields: Vec<MonoExpr>,
    },
    VariantConstruct {
        enum_path: Vec<String>,
        type_args: Vec<RType>,
        disc: u32,
        // Payload exprs, in declared field order.
        payload: Vec<MonoExpr>,
    },
    Tuple(Vec<MonoExpr>),
    Cast {
        inner: Box<MonoExpr>,
        target: RType,
    },
    Match {
        scrutinee: Box<MonoExpr>,
        arms: Vec<MonoArm>,
    },
    // `if`, `while`, `for`, `?`, `&&`, `||`, `if let` are surface
    // constructs that the lowering pass desugars away — they are NOT
    // Mono variants. The AST keeps them so typeck/borrowck can attribute
    // errors to the surface form; lowering emits `Loop`/`Match`/`Break`
    // shapes instead. `if cond { a } else { b }` lowers to
    // `match cond { true => a, false => b }`; `while cond { body }`
    // lowers to `loop { match cond { true => body, false => break } }`;
    // `arr[i]` lowers to `*<Index|IndexMut>::index{,_mut}(&arr, i)`
    // (a Deref-of-MethodCall place; mutability comes from the
    // enclosing borrow/assign context at lowering time).
    Loop {
        label: Option<String>,
        body: Box<MonoBlock>,
    },
    Block(Box<MonoBlock>),
    Unsafe(Box<MonoBlock>),
    Break {
        label: Option<String>,
        value: Option<Box<MonoExpr>>,
    },
    Continue {
        label: Option<String>,
    },
    Return {
        value: Option<Box<MonoExpr>>,
    },
    MacroCall {
        name: String,
        args: Vec<MonoExpr>,
    },
}

// `ReceiverAdjust` is re-exported from typeck for convenience —
// MethodResolution carries one of these and Mono mirrors it.
pub use crate::typeck::ReceiverAdjust;

#[allow(dead_code)]
pub struct MonoArm {
    pub pattern: AstPattern,
    pub guard: Option<MonoExpr>,
    pub body: MonoExpr,
    pub span: Span,
}

#[allow(dead_code)]
pub struct MonoBlock {
    pub stmts: Vec<MonoStmt>,
    pub tail: Option<MonoExpr>,
    pub span: Span,
}

#[allow(dead_code)]
pub enum MonoStmt {
    Let {
        binding: BindingId,
        value: MonoExpr,
        span: Span,
    },
    // `let x: T;` — declared but uninitialized. Allocates storage
    // for the binding (so subsequent assignments can write into it)
    // and pushes it onto codegen's locals, but emits no value.
    // Borrowck has already proven the binding isn't read before its
    // first assignment; codegen leaves the slot's bytes
    // uninitialized.
    LetUninit {
        binding: BindingId,
        span: Span,
    },
    // Pattern destructuring let: `let (a, b) = e;` (irrefutable) or
    // `let Some(x) = e else { … };` (refutable + diverging else block).
    // The pattern's binding leaves stay scoped after the stmt — codegen
    // wires the test (Block(Block(pattern; Br 1)); else; Unreachable;
    // End) so the success path falls through with bindings live.
    LetPattern {
        pattern: AstPattern,
        value: MonoExpr,
        // Some for `let PAT = e else { … };`, None for irrefutable
        // destructure. typeck guarantees the else block diverges (`!`).
        else_block: Option<Box<MonoBlock>>,
        span: Span,
    },
    Assign {
        place: MonoPlace,
        value: MonoExpr,
        span: Span,
    },
    Expr(MonoExpr),
    // Explicit Drop terminator. Inserted by drop-site insertion (a
    // future phase); for now only constructed by an explicit drop pass
    // — Phase 1a's lowering doesn't insert these yet, but the variant
    // exists for the consumer side to be ready.
    Drop {
        binding: BindingId,
        span: Span,
    },
    // Drop-flag clear at a move site (for Flagged bindings).
    ClearDropFlag {
        binding: BindingId,
        span: Span,
    },
}

// Top-level lowered function. Owns its body and locals.
#[allow(dead_code)]
pub struct MonoBody {
    pub locals: Vec<MonoLocal>,
    pub body: MonoBlock,
}

// ============================================================================
// Lowering: typed AST + MonoFn artifacts → MonoBody.
//
// Phase 1a skeleton: handles the simple ExprKind variants and returns
// `Err("...not yet lowered...")` for the complex ones (StructLit, Match,
// IfLet, For, Try, Index, MethodCall, MacroCall, Cast). Phase 1b fills
// these in. Phase 1c invokes from codegen and migrates emission to read
// from MonoBody instead of the AST + side tables.
// ============================================================================

#[allow(dead_code)]
struct LowerCtx<'a> {
    input: &'a MonoFnInput<'a>,
    structs: &'a StructTable,
    enums: &'a EnumTable,
    traits: &'a TraitTable,
    funcs: &'a FuncTable,
    mono_table: &'a MonoTable,
    locals: Vec<MonoLocal>,
    // Name → BindingId stack for lexically-scoped lookup. Walked in
    // reverse to resolve `Var(name)`.
    scope: Vec<(String, BindingId)>,
    // Counter for synthesized binding name uniqueness (`__iter_0`,
    // `__iter_1`, …) so nested For loops don't collide.
    synth_counter: u32,
}

#[allow(dead_code)]
impl<'a> LowerCtx<'a> {
    fn new(
        input: &'a MonoFnInput<'a>,
        structs: &'a StructTable,
        enums: &'a EnumTable,
        traits: &'a TraitTable,
        funcs: &'a FuncTable,
        mono_table: &'a MonoTable,
    ) -> Self {
        Self {
            input,
            structs,
            enums,
            traits,
            funcs,
            mono_table,
            locals: Vec::new(),
            scope: Vec::new(),
            synth_counter: 0,
        }
    }

    fn next_synth_name(&mut self, prefix: &str) -> String {
        let n = self.synth_counter;
        self.synth_counter += 1;
        format!("__{}_{}", prefix, n)
    }

    // Allocate a BindingId, push the MonoLocal, and add the name to the
    // active scope. Returns the new BindingId.
    fn declare_binding(
        &mut self,
        name: String,
        ty: RType,
        origin: BindingOrigin,
    ) -> BindingId {
        let id = self.locals.len() as BindingId;
        self.scope.push((name.clone(), id));
        self.locals.push(MonoLocal { id, name, ty, origin });
        id
    }

    fn lookup(&self, name: &str) -> Option<BindingId> {
        let mut i = self.scope.len();
        while i > 0 {
            i -= 1;
            if self.scope[i].0 == name {
                return Some(self.scope[i].1);
            }
        }
        None
    }

    fn expr_ty(&self, expr: &Expr) -> Result<RType, Error> {
        match &self.input.expr_types[expr.id as usize] {
            Some(t) => Ok(t.clone()),
            None => Err(Error {
                file: String::new(),
                message: format!(
                    "lower_to_mono: no expr_type recorded for expr.id={}",
                    expr.id
                ),
                span: expr.span.copy(),
            }),
        }
    }
}

// Top-level entry: lower a MonoFnInput (AST body + typeck artifacts)
// to a fully-owned MonoFn (lowered MonoBody + signature + drop state).
// The input's typeck caches are consumed here and not stored on the
// returned MonoFn — codegen reads from MonoBody / MonoLayout instead.
#[allow(dead_code)]
pub fn lower_to_mono(
    input: &MonoFnInput,
    structs: &StructTable,
    enums: &EnumTable,
    traits: &TraitTable,
    funcs: &FuncTable,
    mono_table: &MonoTable,
) -> Result<MonoFn, Error> {
    let mut ctx = LowerCtx::new(input, structs, enums, traits, funcs, mono_table);
    // Declare params first, in declaration order. Storage and
    // drop-action decisions are computed by a separate post-lowering
    // layout pass that walks the Mono IR.
    let mut k = 0;
    while k < input.func.params.len() {
        let name = input.func.params[k].name.clone();
        let ty = input.param_types[k].clone();
        ctx.declare_binding(name, ty, BindingOrigin::Param(k));
        k += 1;
    }
    let body_block = lower_block(&mut ctx, &input.func.body)?;
    let body = MonoBody { locals: ctx.locals, body: body_block };
    Ok(MonoFn {
        name: input.func.name.clone(),
        param_types: input.param_types.clone(),
        return_type: input.return_type.clone(),
        body,
        moved_places: input.moved_places.clone(),
        move_sites: input.move_sites.clone(),
        wasm_idx: input.wasm_idx,
        is_export: input.is_export,
    })
}

#[allow(dead_code)]
fn lower_block(ctx: &mut LowerCtx, block: &crate::ast::Block) -> Result<MonoBlock, Error> {
    let scope_mark = ctx.scope.len();
    let mut stmts: Vec<MonoStmt> = Vec::new();
    let mut i = 0;
    while i < block.stmts.len() {
        let s = lower_stmt(ctx, &block.stmts[i])?;
        if let Some(ms) = s {
            stmts.push(ms);
        }
        i += 1;
    }
    let tail = match &block.tail {
        Some(e) => Some(lower_expr(ctx, e)?),
        None => None,
    };
    while ctx.scope.len() > scope_mark {
        ctx.scope.pop();
    }
    Ok(MonoBlock { stmts, tail, span: block.span.copy() })
}

#[allow(dead_code)]
fn lower_stmt(ctx: &mut LowerCtx, stmt: &Stmt) -> Result<Option<MonoStmt>, Error> {
    match stmt {
        Stmt::Use(_) => Ok(None),
        Stmt::Expr(e) => {
            let me = lower_expr(ctx, e)?;
            Ok(Some(MonoStmt::Expr(me)))
        }
        Stmt::Let(ls) => {
            // Uninit `let x: T;` (no value): typeck has already
            // validated single-Binding pattern + present annotation
            // and recorded the resolved type at `pattern.id` (via
            // `check_pattern`). Allocate the binding and emit a
            // `LetUninit`. Borrowck enforces "no read before
            // assign"; codegen reserves storage but emits no value.
            if ls.value.is_none() {
                let (name, _mutable, _name_span) = crate::ast::let_simple_binding(ls)
                    .expect("typeck enforces single Binding for uninit let");
                let pat_id = ls.pattern.id as usize;
                let ty_rt = ctx.input.expr_types.get(pat_id)
                    .and_then(|o| o.as_ref())
                    .cloned()
                    .expect("typeck records uninit let pattern type at pattern.id");
                // `input.expr_types` is already substituted at
                // monomorphization input-build time; no env needed.
                let binding = ctx.declare_binding(
                    name.to_string(),
                    ty_rt,
                    BindingOrigin::LetValue,
                );
                return Ok(Some(MonoStmt::LetUninit {
                    binding,
                    span: ls.pattern.span.copy(),
                }));
            }
            let value_expr = ls.value.as_ref().expect("just checked is_some");
            let value = lower_expr(ctx, value_expr)?;
            // Simple-binding fast path: bare `let x = e;`.
            if let Some((name, _mutable, _name_span)) = crate::ast::let_simple_binding(ls) {
                let ty = value.ty.clone();
                let binding = ctx.declare_binding(
                    name.to_string(),
                    ty,
                    BindingOrigin::LetValue,
                );
                return Ok(Some(MonoStmt::Let { binding, value, span: value_expr.span.copy() }));
            }
            // Complex pattern (tuple destructure, let-else, etc.):
            // allocate BindingIds for each binding leaf so subsequent
            // `Var(name)` lookups in the body resolve. The pattern
            // shape itself is preserved for codegen to bind via
            // `codegen_pattern`. For let-else, lower the else block
            // here too — it must diverge (typeck-verified).
            declare_pattern_bindings(ctx, &ls.pattern)?;
            let else_block = match &ls.else_block {
                Some(eb) => Some(Box::new(lower_block(ctx, eb.as_ref())?)),
                None => None,
            };
            Ok(Some(MonoStmt::LetPattern {
                pattern: desugar_pattern(&ls.pattern, &ctx.input.pattern_ergo),
                value,
                else_block,
                span: value_expr.span.copy(),
            }))
        }
        Stmt::Assign(a) => {
            let place = match lower_place(ctx, &a.lhs, /* mutable */ true)? {
                Some(p) => p,
                None => return Err(Error {
                    file: String::new(),
                    message: "lower_to_mono: assign LHS isn't a place expr".to_string(),
                    span: a.span.copy(),
                }),
            };
            let value = lower_expr(ctx, &a.rhs)?;
            Ok(Some(MonoStmt::Assign { place, value, span: a.span.copy() }))
        }
    }
}

#[allow(dead_code)]
fn lower_expr(ctx: &mut LowerCtx, expr: &Expr) -> Result<MonoExpr, Error> {
    let ty = ctx.expr_ty(expr)?;
    let span = expr.span.copy();
    let kind = match &expr.kind {
        ExprKind::IntLit(n) => MonoExprKind::Lit(MonoLit::Int { magnitude: *n, negated: false }),
        ExprKind::NegIntLit(n) => MonoExprKind::Lit(MonoLit::Int { magnitude: *n, negated: true }),
        ExprKind::BoolLit(b) => MonoExprKind::Lit(MonoLit::Bool(*b)),
        ExprKind::CharLit(c) => MonoExprKind::Lit(MonoLit::Char(*c)),
        ExprKind::StrLit(s) => MonoExprKind::Lit(MonoLit::Str(s.clone())),
        ExprKind::Var(name) => {
            // Fn-item-as-fn-pointer takes priority over local lookup —
            // typeck only records `fn_item_addrs[id]` when the Var
            // resolved to a fn item (no local of the same name).
            if let Some(Some(callee_idx)) = ctx.input.fn_item_addrs.get(expr.id as usize) {
                let wasm_idx = ctx.funcs.entries[*callee_idx].idx;
                MonoExprKind::FnItemAddr { wasm_idx }
            }
            // Const reference takes priority over local lookup —
            // typeck only records `const_uses[id]` when the Var failed
            // to resolve as a local, so the two can't both fire.
            else if let Some(slot) = ctx.input.const_uses.get(expr.id as usize) {
                if let Some(value) = slot {
                    let lit = match value {
                        crate::typeck::ConstValue::Int { magnitude, negated } => {
                            MonoLit::Int { magnitude: *magnitude, negated: *negated }
                        }
                        crate::typeck::ConstValue::Bool(b) => MonoLit::Bool(*b),
                        crate::typeck::ConstValue::Char(c) => MonoLit::Char(*c),
                        crate::typeck::ConstValue::Str(s) => MonoLit::Str(s.clone()),
                    };
                    MonoExprKind::Lit(lit)
                } else {
                    match ctx.lookup(name) {
                        Some(id) => MonoExprKind::Local(id, expr.id),
                        None => return Err(Error {
                            file: String::new(),
                            message: format!("lower_to_mono: no binding in scope for `{}`", name),
                            span,
                        }),
                    }
                }
            } else {
                match ctx.lookup(name) {
                    Some(id) => MonoExprKind::Local(id, expr.id),
                    None => return Err(Error {
                        file: String::new(),
                        message: format!("lower_to_mono: no binding in scope for `{}`", name),
                        span,
                    }),
                }
            }
        }
        ExprKind::Block(b) => MonoExprKind::Block(Box::new(lower_block(ctx, b.as_ref())?)),
        ExprKind::Unsafe(b) => MonoExprKind::Unsafe(Box::new(lower_block(ctx, b.as_ref())?)),
        ExprKind::If(if_expr) => {
            // Desugar `if cond { a } else { b }` to
            // `match cond { true => { a }, false => { b } }`.
            let cond = Box::new(lower_expr(ctx, &if_expr.cond)?);
            let then_block = lower_block(ctx, if_expr.then_block.as_ref())?;
            let else_block = lower_block(ctx, if_expr.else_block.as_ref())?;
            let then_body = MonoExpr {
                kind: MonoExprKind::Block(Box::new(then_block)),
                ty: ty.clone(),
                span: if_expr.then_block.span.copy(),
            };
            let else_body = MonoExpr {
                kind: MonoExprKind::Block(Box::new(else_block)),
                ty: ty.clone(),
                span: if_expr.else_block.span.copy(),
            };
            MonoExprKind::Match {
                scrutinee: cond,
                arms: vec![
                    MonoArm {
                        pattern: synth_bool_pat(true, if_expr.then_block.span.copy()),
                        guard: None,
                        body: then_body,
                        span: if_expr.then_block.span.copy(),
                    },
                    MonoArm {
                        pattern: synth_bool_pat(false, if_expr.else_block.span.copy()),
                        guard: None,
                        body: else_body,
                        span: if_expr.else_block.span.copy(),
                    },
                ],
            }
        }
        ExprKind::While(w) => {
            // Desugar `while cond { body }` to
            // `loop { match cond { true => body, false => break } }`.
            let cond = Box::new(lower_expr(ctx, &w.cond)?);
            let body_block = lower_block(ctx, w.body.as_ref())?;
            let body_span = w.body.span.copy();
            let body_as_expr = MonoExpr {
                kind: MonoExprKind::Block(Box::new(body_block)),
                ty: RType::Tuple(Vec::new()),
                span: body_span.copy(),
            };
            let break_expr = MonoExpr {
                kind: MonoExprKind::Break { label: None, value: None },
                ty: RType::Never,
                span: body_span.copy(),
            };
            let match_expr = MonoExpr {
                kind: MonoExprKind::Match {
                    scrutinee: cond,
                    arms: vec![
                        MonoArm {
                            pattern: synth_bool_pat(true, body_span.copy()),
                            guard: None,
                            body: body_as_expr,
                            span: body_span.copy(),
                        },
                        MonoArm {
                            pattern: synth_bool_pat(false, body_span.copy()),
                            guard: None,
                            body: break_expr,
                            span: body_span.copy(),
                        },
                    ],
                },
                ty: RType::Tuple(Vec::new()),
                span: body_span.copy(),
            };
            let loop_body = MonoBlock {
                stmts: Vec::new(),
                tail: Some(match_expr),
                span: body_span.copy(),
            };
            MonoExprKind::Loop {
                label: w.label.clone(),
                body: Box::new(loop_body),
            }
        }
        ExprKind::Borrow { inner, mutable } => {
            match lower_place(ctx, inner, *mutable)? {
                Some(place) => MonoExprKind::Borrow { place, mutable: *mutable },
                None => {
                    // Borrow of a value-producing expression (literal,
                    // call, etc.). Wrap as BorrowOfValue — codegen
                    // materializes via a shadow-stack slot.
                    let value = lower_expr(ctx, inner)?;
                    MonoExprKind::BorrowOfValue {
                        value: Box::new(value),
                        mutable: *mutable,
                    }
                }
            }
        }
        ExprKind::Tuple(elems) => {
            let mut out = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(lower_expr(ctx, &elems[i])?);
                i += 1;
            }
            MonoExprKind::Tuple(out)
        }
        ExprKind::Builtin { name, args, .. } => {
            let mut out_args = Vec::new();
            let mut i = 0;
            while i < args.len() {
                out_args.push(lower_expr(ctx, &args[i])?);
                i += 1;
            }
            let type_args = match &ctx.input.builtin_type_targets[expr.id as usize] {
                Some(ts) => ts.clone(),
                None => Vec::new(),
            };
            MonoExprKind::Builtin { name: name.clone(), type_args, args: out_args }
        }
        ExprKind::Break { label, .. } => MonoExprKind::Break { label: label.clone(), value: None },
        ExprKind::Continue { label, .. } => MonoExprKind::Continue { label: label.clone() },
        ExprKind::Return { value } => {
            let lowered = match value {
                Some(v) => Some(Box::new(lower_expr(ctx, v)?)),
                None => None,
            };
            MonoExprKind::Return { value: lowered }
        }
        ExprKind::Cast { inner, .. } => {
            let inner = Box::new(lower_expr(ctx, inner)?);
            MonoExprKind::Cast { inner, target: ty.clone() }
        }
        ExprKind::Deref(_) => {
            // Prefer the place-context lowering — it dispatches `*expr`
            // for smart-pointer (struct) types through `synth_deref_call`
            // (yielding `Deref(MethodCall<Deref::deref>(&inner))`), and
            // for Ref/RawPtr through the bare `Deref(PlaceLoad(inner))`
            // shape. Both produce a `Deref` whose inner has Ref/RawPtr
            // type — which is what codegen's `emit_place_address` Deref
            // arm and `mono_supports_place`'s gate require. The naive
            // fallback below would produce `Deref(Local(box_thing))`
            // where inner.ty is a Struct, then mono_supports_place
            // rejects (correctly: codegen treats the Local's flat-load
            // as an address but for a struct it pushes multiple scalars).
            if let Some(place) = lower_place(ctx, expr, /* mutable */ false)? {
                return Ok(MonoExpr {
                    kind: MonoExprKind::PlaceLoad(place),
                    ty: ty.clone(),
                    span: span.copy(),
                });
            }
            // Fallback for non-place inners (rare): keep the raw shape.
            // mono_supports_place + codegen will reject if the chain
            // can't materialize as an address.
            let inner_lowered = lower_expr(ctx, match &expr.kind {
                ExprKind::Deref(i) => i,
                _ => unreachable!(),
            })?;
            let place = MonoPlace {
                kind: MonoPlaceKind::Deref { inner: Box::new(inner_lowered) },
                ty: ty.clone(),
                span: span.copy(),
            };
            MonoExprKind::PlaceLoad(place)
        }
        ExprKind::FieldAccess(fa) => {
            // Prefer the place-context lowering: it inserts explicit
            // Deref nodes when the base is Ref/RawPtr/smart-pointer
            // typed, and resolves the field offset against the
            // post-deref struct type. The value-context fallback below
            // produces `Field { base: Local(self_ref), byte_offset: 0 }`
            // — which auto-derefs correctly only for the FIRST field of
            // the pointee (where the field offset happens to be 0); all
            // other fields silently miscompile through `emit_place_address`
            // because the "address" model treats `Local(self_ref)` as
            // pointing at self, not at *self.
            if let Some(place) = lower_place(ctx, expr, /* mutable */ false)? {
                return Ok(MonoExpr {
                    kind: MonoExprKind::PlaceLoad(place),
                    ty: ty.clone(),
                    span: span.copy(),
                });
            }
            // Fallback for non-place bases (e.g. `call().field`): the
            // base is a value-producing expr; lowering wraps as a
            // synthesized place via `expr_to_place_kind_or_temp`. Field
            // offset is best-effort (codegen typically falls back to AST
            // anyway).
            let base = lower_expr(ctx, &fa.base)?;
            let base_ty = base.ty.clone();
            let byte_offset = match resolve_field_info(ctx, &base_ty, &fa.field) {
                Some((_, off)) => off,
                None => 0,
            };
            let place = MonoPlace {
                kind: MonoPlaceKind::Field {
                    base: Box::new(MonoPlace {
                        kind: expr_to_place_kind_or_temp(base),
                        ty: base_ty,
                        span: fa.base.span.copy(),
                    }),
                    field_name: fa.field.clone(),
                    byte_offset,
                },
                ty: ty.clone(),
                span: span.copy(),
            };
            MonoExprKind::PlaceLoad(place)
        }
        ExprKind::TupleIndex { base, index, .. } => {
            let base_lowered = lower_expr(ctx, base)?;
            let base_ty = base_lowered.ty.clone();
            let byte_offset = resolve_tuple_offset(ctx, &base_ty, *index).unwrap_or(0);
            let place = MonoPlace {
                kind: MonoPlaceKind::TupleIndex {
                    base: Box::new(MonoPlace {
                        kind: expr_to_place_kind_or_temp(base_lowered),
                        ty: base_ty,
                        span: base.span.copy(),
                    }),
                    index: *index,
                    byte_offset,
                },
                ty: ty.clone(),
                span: span.copy(),
            };
            MonoExprKind::PlaceLoad(place)
        }
        ExprKind::MacroCall { name, args, .. } => {
            let mut out_args = Vec::new();
            let mut i = 0;
            while i < args.len() {
                out_args.push(lower_expr(ctx, &args[i])?);
                i += 1;
            }
            MonoExprKind::MacroCall { name: name.clone(), args: out_args }
        }
        ExprKind::Call(c) => {
            // Bare-dyn-fn-call sugar: `f(args)` where `f: &dyn Fn(...)
            // -> R` (or `&mut dyn FnMut`, or `Box<dyn Fn>`). typeck
            // records the callee binding on `bare_closure_calls[id]`
            // AND a `dyn_method_calls[id]` slot. Lower as DynMethodCall
            // — recv is the dyn fat ref (the binding's value), args is
            // a single-element vec wrapping a tuple of the original
            // args (matching the `Fn::call(&self, args: Args)` shape).
            if let Some(Some(dmd)) = ctx.input.dyn_method_calls.get(expr.id as usize) {
                if let Some(binding_name) =
                    ctx.input.bare_closure_calls.get(expr.id as usize).and_then(|o| o.as_ref())
                {
                    let recv_binding = ctx.lookup(binding_name).ok_or_else(|| Error {
                        file: String::new(),
                        message: format!(
                            "lower_to_mono: bare-dyn-fn-call recv `{}` not in scope",
                            binding_name
                        ),
                        span: span.copy(),
                    })?;
                    let recv_ty = ctx.locals[recv_binding as usize].ty.clone();
                    let recv = MonoExpr {
                        kind: MonoExprKind::Local(recv_binding, u32::MAX),
                        ty: recv_ty,
                        span: span.copy(),
                    };
                    // Pack the call args into a tuple — Fn::call takes
                    // a single `Args` tuple param after &self.
                    let mut tuple_elems: Vec<MonoExpr> = Vec::new();
                    let mut tuple_elem_types: Vec<RType> = Vec::new();
                    let mut i = 0;
                    while i < c.args.len() {
                        let lowered = lower_expr(ctx, &c.args[i])?;
                        tuple_elem_types.push(lowered.ty.clone());
                        tuple_elems.push(lowered);
                        i += 1;
                    }
                    let args_tuple_ty = RType::Tuple(tuple_elem_types);
                    let args_tuple = MonoExpr {
                        kind: MonoExprKind::Tuple(tuple_elems),
                        ty: args_tuple_ty,
                        span: span.copy(),
                    };
                    return Ok(MonoExpr {
                        kind: MonoExprKind::DynMethodCall {
                            recv: Box::new(recv),
                            method_idx: dmd.method_idx,
                            args: vec![args_tuple],
                            method_param_types: dmd.method_param_types.clone(),
                            method_return_type: dmd.method_return_type.clone(),
                            recv_mut: dmd.recv_mut,
                            trait_path: dmd.trait_path.clone(),
                        },
                        ty,
                        span: expr.span.copy(),
                    });
                }
            }
            // Bare-closure-call sugar: typeck recorded the callee
            // binding name on `bare_closure_calls[id]` when this Call
            // dispatched as `local.call((args,))`. Lower as a
            // MethodCall MonoExpr — recv is the closure local, args
            // is a single-element vec wrapping a tuple of the
            // original args. The trait_dispatch on
            // method_resolutions[id] points codegen at the
            // synthesized `Fn::call` impl method registered post-
            // typeck by `closure_lower::lower`.
            if let Some(binding_name) =
                ctx.input.bare_closure_calls.get(expr.id as usize).and_then(|o| o.as_ref())
            {
                let recv_binding = ctx.lookup(binding_name).ok_or_else(|| Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: bare-closure-call recv `{}` not in scope",
                        binding_name
                    ),
                    span: span.copy(),
                })?;
                let mut tuple_elems: Vec<MonoExpr> = Vec::new();
                let mut tuple_elem_types: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < c.args.len() {
                    let lowered = lower_expr(ctx, &c.args[i])?;
                    tuple_elem_types.push(lowered.ty.clone());
                    tuple_elems.push(lowered);
                    i += 1;
                }
                let args_tuple_ty = RType::Tuple(tuple_elem_types);
                let args_tuple = MonoExpr {
                    kind: MonoExprKind::Tuple(tuple_elems),
                    ty: args_tuple_ty,
                    span: span.copy(),
                };
                // Resolve the wasm idx via trait_dispatch on the
                // matching method_resolutions entry (typeck populated
                // it during check_bare_closure_call).
                let mr = ctx.input.method_resolutions[expr.id as usize]
                    .as_ref()
                    .ok_or_else(|| Error {
                        file: String::new(),
                        message: "lower_to_mono: bare-closure-call missing method_resolution"
                            .to_string(),
                        span: span.copy(),
                    })?;
                let td = mr.trait_dispatch.as_ref().ok_or_else(|| Error {
                    file: String::new(),
                    message: "lower_to_mono: bare-closure-call missing trait_dispatch"
                        .to_string(),
                    span: span.copy(),
                })?;
                let wasm_idx = resolve_trait_dispatch_method(
                    ctx,
                    td,
                    &mr.type_args,
                    &span,
                )?;
                let recv_ty = ctx.locals[recv_binding as usize].ty.clone();
                let recv_expr = MonoExpr {
                    kind: MonoExprKind::Local(recv_binding, expr.id),
                    ty: recv_ty,
                    span: span.copy(),
                };
                MonoExprKind::MethodCall {
                    wasm_idx,
                    recv_adjust: mr.recv_adjust,
                    recv: Box::new(recv_expr),
                    args: vec![args_tuple],
                }
            } else {
            // Lower args first.
            let mut out_args = Vec::new();
            let mut i = 0;
            while i < c.args.len() {
                out_args.push(lower_expr(ctx, &c.args[i])?);
                i += 1;
            }
            // Resolve callee from typeck's CallResolution.
            let resolution = ctx.input.call_resolutions[expr.id as usize].as_ref();
            let resolution = match resolution {
                Some(r) => r,
                None => return Err(Error {
                    file: String::new(),
                    message: "lower_to_mono: no call_resolution recorded".to_string(),
                    span,
                }),
            };
            match resolution {
                CallResolution::Direct(idx) => {
                    let wasm_idx = ctx.funcs.entries[*idx].idx;
                    MonoExprKind::Call { wasm_idx, args: out_args }
                }
                CallResolution::Generic { template_idx, type_args } => {
                    // type_args are already substituted via build_mono_for_template.
                    let wasm_idx = match ctx.mono_table.lookup(*template_idx, type_args) {
                        Some(idx) => idx,
                        None => return Err(Error {
                            file: String::new(),
                            message: format!(
                                "lower_to_mono: mono_table missing entry for template {} (expand should have interned)",
                                template_idx
                            ),
                            span,
                        }),
                    };
                    MonoExprKind::Call { wasm_idx, args: out_args }
                }
                CallResolution::Variant { enum_path, disc, type_args } => {
                    MonoExprKind::VariantConstruct {
                        enum_path: enum_path.clone(),
                        type_args: type_args.clone(),
                        disc: *disc,
                        payload: out_args,
                    }
                }
                CallResolution::Indirect { callee_local_name, fn_ptr_ty } => {
                    // Resolve the callee local by name to a BindingId,
                    // then emit a CallIndirect. The callee's value (an
                    // i32 table slot) lowers via the standard
                    // `MonoExprKind::Local` path. The signature drives
                    // the typeidx at codegen.
                    let binding_id = match ctx.lookup(callee_local_name) {
                        Some(id) => id,
                        None => return Err(Error {
                            file: String::new(),
                            message: format!(
                                "lower_to_mono: callee local `{}` not found in scope",
                                callee_local_name
                            ),
                            span,
                        }),
                    };
                    let callee_ty = fn_ptr_ty.clone();
                    let callee = MonoExpr {
                        kind: MonoExprKind::Local(binding_id, u32::MAX),
                        ty: callee_ty,
                        span: span.copy(),
                    };
                    MonoExprKind::CallIndirect {
                        callee: Box::new(callee),
                        args: out_args,
                        fn_ptr_ty: fn_ptr_ty.clone(),
                    }
                }
            }
            }
        }
        ExprKind::MethodCall(mc) => {
            // Dyn-method dispatch takes priority over the standard
            // method-resolution path: typeck records on
            // `dyn_method_calls[expr.id]` only when the receiver was
            // `&dyn Trait` / `&mut dyn Trait`, and the standard
            // `method_resolutions[expr.id]` is empty in that case.
            if let Some(Some(dmd)) = ctx.input.dyn_method_calls.get(expr.id as usize) {
                // Lower the receiver as a value (yields the fat ref's
                // two i32 scalars).
                let recv = Box::new(lower_expr(ctx, &mc.receiver)?);
                let mut out_args = Vec::new();
                let mut i = 0;
                while i < mc.args.len() {
                    out_args.push(lower_expr(ctx, &mc.args[i])?);
                    i += 1;
                }
                return Ok(MonoExpr {
                    kind: MonoExprKind::DynMethodCall {
                        recv,
                        method_idx: dmd.method_idx,
                        args: out_args,
                        method_param_types: dmd.method_param_types.clone(),
                        method_return_type: dmd.method_return_type.clone(),
                        recv_mut: dmd.recv_mut,
                        trait_path: dmd.trait_path.clone(),
                    },
                    ty,
                    span: expr.span.copy(),
                });
            }
            // Resolve method dispatch first so we know recv_adjust
            // before lowering the receiver. Mutability of the autoref
            // (BorrowImm vs BorrowMut) flows into `lower_place` so an
            // inner `arr[i]` receiver picks the right `Index`/`IndexMut`
            // variant — `v[0].add_assign(1)` needs `index_mut`, not
            // `index` (which would return `&u32` and silently let
            // add_assign write through what borrowck should reject).
            let resolution = ctx.input.method_resolutions[expr.id as usize].as_ref();
            let resolution = match resolution {
                Some(r) => r,
                None => return Err(Error {
                    file: String::new(),
                    message: "lower_to_mono: no method_resolution recorded".to_string(),
                    span,
                }),
            };
            let recv_adjust = resolution.recv_adjust;
            let recv = match recv_adjust {
                ReceiverAdjust::BorrowMut | ReceiverAdjust::BorrowImm => {
                    let mutable = matches!(recv_adjust, ReceiverAdjust::BorrowMut);
                    match lower_place(ctx, &mc.receiver, mutable)? {
                        Some(place) => {
                            let pty = place.ty.clone();
                            let pspan = place.span.copy();
                            Box::new(MonoExpr {
                                kind: MonoExprKind::PlaceLoad(place),
                                ty: pty,
                                span: pspan,
                            })
                        }
                        None => Box::new(lower_expr(ctx, &mc.receiver)?),
                    }
                }
                _ => Box::new(lower_expr(ctx, &mc.receiver)?),
            };
            let mut out_args = Vec::new();
            let mut i = 0;
            while i < mc.args.len() {
                out_args.push(lower_expr(ctx, &mc.args[i])?);
                i += 1;
            }
            let wasm_idx = if let Some(td) = &resolution.trait_dispatch {
                resolve_trait_dispatch_method(ctx, td, &resolution.type_args, &span)?
            } else {
                match resolution.template_idx {
                    Some(template_idx) => {
                        match ctx.mono_table.lookup(template_idx, &resolution.type_args) {
                            Some(idx) => idx,
                            None => return Err(Error {
                                file: String::new(),
                                message: format!(
                                    "lower_to_mono: mono_table missing entry for method template {}",
                                    template_idx
                                ),
                                span,
                            }),
                        }
                    }
                    None => resolution.callee_idx,
                }
            };
            MonoExprKind::MethodCall { wasm_idx, recv_adjust, recv, args: out_args }
        }
        ExprKind::StructLit(s) => {
            // Two cases share AST `StructLit` syntax:
            //   - Plain struct: `Foo { a: 1 }` — `ty` is `RType::Struct`.
            //   - Struct-form enum variant: `Shape::Rect { w: 6, h: 7 }`
            //     — `ty` is `RType::Enum`; typeck recorded a
            //     `CallResolution::Variant` at this expr's id. Lower as
            //     a `VariantConstruct` with payload reordered to the
            //     variant's declared field order.
            if let Some(crate::typeck::CallResolution::Variant {
                enum_path, disc, type_args,
            }) = ctx.input.call_resolutions[expr.id as usize].as_ref() {
                let enum_path = enum_path.clone();
                let disc = *disc;
                let type_args = type_args.clone();
                // Look up the variant's declared field order to
                // reorder the literal's named-field initializers.
                let field_order = {
                    let entry = match crate::typeck::enum_lookup(ctx.enums, &enum_path) {
                        Some(e) => e,
                        None => return Err(Error {
                            file: String::new(),
                            message: format!(
                                "lower_to_mono: enum {:?} not found",
                                enum_path
                            ),
                            span,
                        }),
                    };
                    let variant = entry.variants.iter()
                        .find(|v| v.disc == disc)
                        .ok_or_else(|| Error {
                            file: String::new(),
                            message: format!(
                                "lower_to_mono: variant disc {} of {:?} not found",
                                disc, enum_path
                            ),
                            span: span.copy(),
                        })?;
                    match &variant.payload {
                        crate::typeck::VariantPayloadResolved::Struct(fields) => {
                            fields.iter().map(|f| f.name.clone()).collect::<Vec<String>>()
                        }
                        _ => return Err(Error {
                            file: String::new(),
                            message: format!(
                                "lower_to_mono: variant disc {} of {:?} isn't struct-shaped",
                                disc, enum_path
                            ),
                            span,
                        }),
                    }
                };
                let mut payload_out: Vec<MonoExpr> = Vec::with_capacity(field_order.len());
                let mut i = 0;
                while i < field_order.len() {
                    let declared_name = &field_order[i];
                    let mut found_idx: Option<usize> = None;
                    let mut k = 0;
                    while k < s.fields.len() {
                        if &s.fields[k].name == declared_name {
                            found_idx = Some(k);
                            break;
                        }
                        k += 1;
                    }
                    let init_idx = match found_idx {
                        Some(k) => k,
                        None => return Err(Error {
                            file: String::new(),
                            message: format!(
                                "lower_to_mono: missing field `{}` in variant lit",
                                declared_name
                            ),
                            span,
                        }),
                    };
                    payload_out.push(lower_expr(ctx, &s.fields[init_idx].value)?);
                    i += 1;
                }
                return Ok(MonoExpr {
                    kind: MonoExprKind::VariantConstruct {
                        enum_path,
                        type_args,
                        disc,
                        payload: payload_out,
                    },
                    ty: ty.clone(),
                    span: span.copy(),
                });
            }
            // Resolve the struct's canonical path + type_args from the
            // expr's resolved type.
            let (path, type_args) = match &ty {
                RType::Struct { path, type_args, .. } => (path.clone(), type_args.clone()),
                _ => return Err(Error {
                    file: String::new(),
                    message: "lower_to_mono: StructLit type isn't Struct".to_string(),
                    span,
                }),
            };
            // Look up the struct entry to get declared field order.
            let entry = match crate::typeck::struct_lookup(ctx.structs, &path) {
                Some(e) => e,
                None => return Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: struct {:?} not found",
                        path
                    ),
                    span,
                }),
            };
            // Reorder field initializers to match declared order.
            let mut fields_out: Vec<MonoExpr> = Vec::with_capacity(entry.fields.len());
            let mut i = 0;
            while i < entry.fields.len() {
                let declared_name = &entry.fields[i].name;
                let mut found_idx: Option<usize> = None;
                let mut k = 0;
                while k < s.fields.len() {
                    if &s.fields[k].name == declared_name {
                        found_idx = Some(k);
                        break;
                    }
                    k += 1;
                }
                let init_idx = match found_idx {
                    Some(k) => k,
                    None => return Err(Error {
                        file: String::new(),
                        message: format!(
                            "lower_to_mono: missing field `{}` in struct lit",
                            declared_name
                        ),
                        span,
                    }),
                };
                fields_out.push(lower_expr(ctx, &s.fields[init_idx].value)?);
                i += 1;
            }
            MonoExprKind::StructLit { path, type_args, fields: fields_out }
        }
        ExprKind::Index { base, index, .. } => {
            // Value position: `arr[i]` desugars to `*<Index>::index(&arr, i)`.
            // The method returns `&Output`; the surrounding Deref makes
            // the place-load yield `Output`. Codegen sees only the
            // standard MethodCall + Deref-of-MethodCall shapes.
            let call = synth_index_call(ctx, base, index, /* mutable */ false, &ty, &span)?;
            MonoExprKind::PlaceLoad(MonoPlace {
                kind: MonoPlaceKind::Deref { inner: Box::new(call) },
                ty: ty.clone(),
                span: span.copy(),
            })
        }
        ExprKind::Match(m) => {
            let scrutinee = Box::new(lower_expr(ctx, &m.scrutinee)?);
            let mut arms = Vec::with_capacity(m.arms.len());
            let mut i = 0;
            while i < m.arms.len() {
                let arm = &m.arms[i];
                // Push pattern bindings into scope so guard/body can
                // resolve them. Pop on arm exit.
                let scope_mark = ctx.scope.len();
                declare_pattern_bindings(ctx, &arm.pattern)?;
                let guard = match &arm.guard {
                    Some(g) => Some(lower_expr(ctx, g)?),
                    None => None,
                };
                let body = lower_expr(ctx, &arm.body)?;
                while ctx.scope.len() > scope_mark {
                    ctx.scope.pop();
                }
                arms.push(MonoArm {
                    pattern: desugar_pattern(&arm.pattern, &ctx.input.pattern_ergo),
                    guard,
                    body,
                    span: arm.span.copy(),
                });
                i += 1;
            }
            MonoExprKind::Match { scrutinee, arms }
        }
        ExprKind::IfLet(il) => {
            // Desugar `if let PAT = scrut { then } else { else }` to
            // `match scrut { PAT => then, _ => else }`. The wildcard arm
            // is synthesized; pattern's span is reused.
            let scrutinee = Box::new(lower_expr(ctx, &il.scrutinee)?);
            // Pattern bindings must be in scope when lowering then_block
            // (mirrors the Match-arm path above). The else_block doesn't
            // see them.
            let scope_mark = ctx.scope.len();
            declare_pattern_bindings(ctx, &il.pattern)?;
            let then_block = lower_block(ctx, il.then_block.as_ref())?;
            while ctx.scope.len() > scope_mark {
                ctx.scope.pop();
            }
            let else_block = lower_block(ctx, il.else_block.as_ref())?;
            let then_body = MonoExpr {
                kind: MonoExprKind::Block(Box::new(then_block)),
                ty: ty.clone(),
                span: il.then_block.span.copy(),
            };
            let else_body = MonoExpr {
                kind: MonoExprKind::Block(Box::new(else_block)),
                ty: ty.clone(),
                span: il.else_block.span.copy(),
            };
            let wildcard_pat = AstPattern {
                kind: crate::ast::PatternKind::Wildcard,
                span: il.pattern.span.copy(),
                id: il.pattern.id, // reuse — won't be queried by codegen for wildcard
            };
            MonoExprKind::Match {
                scrutinee,
                arms: vec![
                    MonoArm {
                        pattern: desugar_pattern(&il.pattern, &ctx.input.pattern_ergo),
                        guard: None,
                        body: then_body,
                        span: il.then_block.span.copy(),
                    },
                    MonoArm {
                        pattern: wildcard_pat,
                        guard: None,
                        body: else_body,
                        span: il.else_block.span.copy(),
                    },
                ],
            }
        }
        ExprKind::For(f) => lower_for(ctx, f, &ty, &span)?,
        ExprKind::Try { inner, question_span } => {
            lower_try(ctx, inner, &ty, question_span, &span)?
        }
        ExprKind::Closure(_) => {
            unreachable!("closure expressions rejected at typeck before mono")
        }
    };
    // If typeck recorded a dyn coercion on this expr id, wrap the
    // inner expression in RefDynCoerce. The inner's runtime value is
    // a single i32 (either the source `&T`'s data ptr or the source
    // `Box<T>`'s heap ptr); the wrapper provides the second word
    // (vtable address) at codegen.
    if let Some(Some(dc)) = ctx.input.dyn_coercions.get(expr.id as usize) {
        // The inner type depends on the coercion shape:
        //   Ref:      inner is `&T` (or `&mut T`) — single i32.
        //   BoxOwned: inner is `Box<T>` — single i32 (the heap ptr).
        let inner_ty = match dc.kind {
            crate::typeck::DynCoercionKind::Ref => {
                let outer_mut = matches!(&ty, RType::Ref { mutable: true, .. });
                let inner_lifetime = match &ty {
                    RType::Ref { lifetime, .. } => lifetime.clone(),
                    _ => crate::typeck::LifetimeRepr::Inferred(0),
                };
                RType::Ref {
                    inner: Box::new(dc.src_concrete_ty.clone()),
                    mutable: outer_mut,
                    lifetime: inner_lifetime,
                }
            }
            crate::typeck::DynCoercionKind::BoxOwned => RType::Struct {
                path: vec!["std".to_string(), "boxed".to_string(), "Box".to_string()],
                type_args: vec![dc.src_concrete_ty.clone()],
                lifetime_args: Vec::new(),
            },
        };
        let inner_ref_expr = MonoExpr {
            kind,
            ty: inner_ty,
            span: expr.span.copy(),
        };
        return Ok(MonoExpr {
            kind: MonoExprKind::RefDynCoerce {
                inner_ref: Box::new(inner_ref_expr),
                src_concrete_ty: dc.src_concrete_ty.clone(),
                bounds: dc.bounds.clone(),
            },
            ty,
            span: expr.span.copy(),
        });
    }
    Ok(MonoExpr { kind, ty, span: expr.span.copy() })
}

// Try to interpret an AST expression as a place (lvalue). Returns
// `Some(MonoPlace)` for `Var(name)` (resolves to `Local(BindingId)`),
// `FieldAccess`, `TupleIndex`, `Deref`, and `Index` chains; `None`
// for value-producing expressions like calls or literals (which a
// borrow would have to materialize into a temp).
//
// `mutable` controls Index dispatch (and propagates through nested
// FieldAccess/TupleIndex bases): true → `IndexMut::index_mut`,
// false → `Index::index`. Caller passes the outermost borrow's
// mutability. For Var/Field/TupleIndex/Deref shapes the flag is
// pass-through; for Index it picks the trait method.
#[allow(dead_code)]
fn lower_place(ctx: &mut LowerCtx, expr: &Expr, mutable: bool) -> Result<Option<MonoPlace>, Error> {
    // Note: we don't read `expr_types[expr.id]` here because typeck
    // skips recording types for some lhs-of-assign positions (Var-rooted
    // chains are walked structurally, not value-typed). We derive each
    // place's type from the binding/inner instead.
    let span = expr.span.copy();
    match &expr.kind {
        ExprKind::Var(name) => {
            match ctx.lookup(name) {
                Some(id) => {
                    let ty = ctx.locals[id as usize].ty.clone();
                    Ok(Some(MonoPlace {
                        kind: MonoPlaceKind::Local(id),
                        ty,
                        span,
                    }))
                }
                None => Ok(None),
            }
        }
        ExprKind::FieldAccess(fa) => {
            let base = match lower_place(ctx, &fa.base, mutable)? {
                Some(p) => p,
                None => return Ok(None),
            };
            // Auto-deref chain: walk `Ref`/`RawPtr`/Deref-impl bases
            // until we reach a struct that has the field. Each deref
            // step inserts an explicit `MonoPlaceKind::Deref` into the
            // place tree — codegen sees structurally-explicit derefs
            // and never has to infer.
            let unwrapped = auto_deref_until_field(ctx, base, &fa.field, &span)?;
            let (ty, byte_offset) = match resolve_field_info(ctx, &unwrapped.ty, &fa.field) {
                Some(p) => p,
                None => return Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: field `{}` not found on {}",
                        fa.field,
                        crate::typeck::rtype_to_string(&unwrapped.ty)
                    ),
                    span,
                }),
            };
            Ok(Some(MonoPlace {
                kind: MonoPlaceKind::Field {
                    base: Box::new(unwrapped),
                    field_name: fa.field.clone(),
                    byte_offset,
                },
                ty,
                span,
            }))
        }
        ExprKind::TupleIndex { base, index, .. } => {
            let base = match lower_place(ctx, base, mutable)? {
                Some(p) => p,
                None => return Ok(None),
            };
            // Auto-deref through Ref/RawPtr to reach the tuple.
            // Smart-pointer auto-deref to a tuple is unusual but the
            // mechanism is the same.
            let unwrapped = auto_deref_until_tuple(ctx, base, *index, &span)?;
            let elem_ty = match &unwrapped.ty {
                RType::Tuple(elems) if (*index as usize) < elems.len() => {
                    elems[*index as usize].clone()
                }
                _ => return Err(Error {
                    file: String::new(),
                    message: format!("lower_to_mono: tuple-index .{} on non-tuple", index),
                    span,
                }),
            };
            let byte_offset = match resolve_tuple_offset(ctx, &unwrapped.ty, *index) {
                Some(o) => o,
                None => return Err(Error {
                    file: String::new(),
                    message: format!("lower_to_mono: tuple-index .{} out of range", index),
                    span,
                }),
            };
            Ok(Some(MonoPlace {
                kind: MonoPlaceKind::TupleIndex {
                    base: Box::new(unwrapped),
                    index: *index,
                    byte_offset,
                },
                ty: elem_ty,
                span,
            }))
        }
        ExprKind::Deref(inner) => {
            // `*expr` — could be raw deref (Ref/RawPtr) or smart-pointer
            // deref via the Deref trait. For raw deref the inner already
            // evaluates to a ref pointer (i32 address). For smart-pointer
            // deref we synthesize a `Deref::deref(&inner)` MethodCall
            // whose return value is a `&Target` ref pointer.
            let inner_lowered = lower_expr(ctx, inner)?;
            let inner_ty = inner_lowered.ty.clone();
            match &inner_ty {
                RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => {
                    let ty = (**inner).clone();
                    Ok(Some(MonoPlace {
                        kind: MonoPlaceKind::Deref { inner: Box::new(inner_lowered) },
                        ty,
                        span,
                    }))
                }
                RType::Struct { .. } => {
                    // Smart-pointer deref: synth Deref::deref method
                    // call. Inner evaluates to a value (not address);
                    // the method call takes `&inner` per recv_adjust.
                    let (deref_call_expr, target_ty) =
                        synth_deref_call(ctx, inner_lowered, &span)?;
                    Ok(Some(MonoPlace {
                        kind: MonoPlaceKind::Deref { inner: Box::new(deref_call_expr) },
                        ty: target_ty,
                        span,
                    }))
                }
                _ => Err(Error {
                    file: String::new(),
                    message: "lower_to_mono: deref of non-pointer non-struct".to_string(),
                    span,
                }),
            }
        }
        ExprKind::Index { base, index, .. } => {
            // `arr[i]` as a place desugars to `*<Index|IndexMut>::index{,_mut}(&arr, i)`.
            // The MethodCall returns `&Output` (or `&mut Output`); wrapping
            // it in `MonoPlace::Deref` produces the addressable element.
            let output_ty = ctx.expr_ty(expr)?;
            let call = synth_index_call(ctx, base, index, mutable, &output_ty, &span)?;
            Ok(Some(MonoPlace {
                kind: MonoPlaceKind::Deref { inner: Box::new(call) },
                ty: output_ty,
                span,
            }))
        }
        _ => Ok(None),
    }
}

// Look up a field's type and byte offset within a struct (auto-deref'ing
// through a ref wrapper if present). Returns None if the type isn't a
// struct or the field doesn't exist.
#[allow(dead_code)]
fn resolve_field_info(
    ctx: &LowerCtx,
    base_ty: &RType,
    field_name: &str,
) -> Option<(RType, u32)> {
    let base_ty = match base_ty {
        RType::Ref { inner, .. } => (**inner).clone(),
        other => other.clone(),
    };
    let (path, type_args) = match &base_ty {
        RType::Struct { path, type_args, .. } => (path, type_args),
        _ => return None,
    };
    let entry = crate::typeck::struct_lookup(ctx.structs, path)?;
    let mut env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < entry.type_params.len() && i < type_args.len() {
        env.push((entry.type_params[i].clone(), type_args[i].clone()));
        i += 1;
    }
    let mut byte_off: u32 = 0;
    let mut k = 0;
    while k < entry.fields.len() {
        let fty = subst_and_peel(&entry.fields[k].ty, &env, ctx.funcs);
        if entry.fields[k].name == field_name {
            return Some((fty, byte_off));
        }
        byte_off += crate::typeck::byte_size_of(&fty, ctx.structs, ctx.enums);
        k += 1;
    }
    None
}

// Return the byte offset of the `index`-th element of a tuple type.
#[allow(dead_code)]
fn resolve_tuple_offset(ctx: &LowerCtx, base_ty: &RType, index: u32) -> Option<u32> {
    let base_ty = match base_ty {
        RType::Ref { inner, .. } => (**inner).clone(),
        other => other.clone(),
    };
    let elems = match &base_ty {
        RType::Tuple(elems) => elems,
        _ => return None,
    };
    if (index as usize) >= elems.len() {
        return None;
    }
    let mut off: u32 = 0;
    let mut i = 0;
    while i < index as usize {
        off += crate::typeck::byte_size_of(&elems[i], ctx.structs, ctx.enums);
        i += 1;
    }
    Some(off)
}

// Helper: when we lowered an expression and want it as a "synthetic"
// MonoPlace base for FieldAccess/TupleIndex, wrap it via a Deref-style
// place if it's a load-form, or as-is. For Phase 1b, we approximate by
// extracting the underlying place if the expression is itself a
// PlaceLoad; otherwise wrap as a synthetic Deref (which is wrong for
// non-pointer values but acts as a placeholder until Phase 1c provides
// proper handling).
#[allow(dead_code)]
fn expr_to_place_kind_or_temp(expr: MonoExpr) -> MonoPlaceKind {
    match expr.kind {
        MonoExprKind::PlaceLoad(p) => p.kind,
        MonoExprKind::Local(id, _) => MonoPlaceKind::Local(id),
        other => {
            // Value-producing expression (e.g. `call().field`). Spill
            // the value to a fresh shadow-stack slot via `BorrowOfValue`
            // (which yields the slot's address), then `Deref` it back to
            // an addressable place. Codegen sees a `Deref { inner: ref }`
            // chain and lowers it correctly.
            let value_ty = expr.ty.clone();
            let span = expr.span.copy();
            let ref_ty = RType::Ref {
                inner: Box::new(value_ty.clone()),
                mutable: false,
                lifetime: crate::typeck::LifetimeRepr::Inferred(0),
            };
            let value_expr = MonoExpr {
                kind: other,
                ty: value_ty,
                span: span.copy(),
            };
            let borrow_expr = MonoExpr {
                kind: MonoExprKind::BorrowOfValue {
                    value: Box::new(value_expr),
                    mutable: false,
                },
                ty: ref_ty,
                span: span.copy(),
            };
            MonoPlaceKind::Deref {
                inner: Box::new(borrow_expr),
            }
        }
    }
}

// Resolve an Index/IndexMut trait method to a concrete wasm function
// index. Uses solve_impl_with_args + find_trait_impl_method, mirroring
// codegen's resolve_index_callee logic.
#[allow(dead_code)]
fn resolve_index_method(
    ctx: &LowerCtx,
    trait_path: &Vec<String>,
    trait_args: &Vec<RType>,
    recv_ty: &RType,
    method_name: &str,
) -> Result<u32, Error> {
    let resolution = match solve_impl_with_args(trait_path, trait_args, recv_ty, ctx.traits, 0) {
        Some(r) => r,
        None => return Err(Error {
            file: String::new(),
            message: format!(
                "lower_to_mono: no impl of {} for {}",
                crate::typeck::place_to_string(trait_path),
                crate::typeck::rtype_to_string(recv_ty)
            ),
            span: crate::span::Span::new(
                crate::span::Pos::new(1, 1),
                crate::span::Pos::new(1, 1),
            ),
        }),
    };
    let cand = match find_trait_impl_method(ctx.funcs, resolution.impl_idx, method_name) {
        Some(c) => c,
        None => return Err(Error {
            file: String::new(),
            message: format!("lower_to_mono: impl missing method `{}`", method_name),
            span: crate::span::Span::new(
                crate::span::Pos::new(1, 1),
                crate::span::Pos::new(1, 1),
            ),
        }),
    };
    match cand {
        MethodCandidate::Direct(i) => Ok(ctx.funcs.entries[i].idx),
        MethodCandidate::Template(i) => {
            let tmpl = &ctx.funcs.templates[i];
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
                concrete.push(found.expect("impl-param bound by subst"));
                k += 1;
            }
            // Index/IndexMut have no method-level type params.
            match ctx.mono_table.lookup(i, &concrete) {
                Some(idx) => Ok(idx),
                None => Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: mono_table missing entry for index method template {}",
                        i
                    ),
                    span: crate::span::Span::new(
                        crate::span::Pos::new(1, 1),
                        crate::span::Pos::new(1, 1),
                    ),
                }),
            }
        }
    }
}

// Synthesize a `true` / `false` pattern node for desugars (If → Match,
// While → Loop+Match). Reuses `Pattern.id = 0` since these synthesized
// patterns aren't in the source's NodeId space — codegen for bool match
// patterns doesn't query expr_types[pat.id], so reuse is safe.
#[allow(dead_code)]
fn synth_bool_pat(value: bool, span: crate::span::Span) -> AstPattern {
    AstPattern {
        kind: crate::ast::PatternKind::LitBool(value),
        span,
        id: 0,
    }
}

// Resolve a trait-dispatched method call to a concrete wasm function
// index. Mirrors codegen::codegen_trait_method_call's resolution
// (solve_impl_with_args + find_trait_impl_method + impl-arg + method-arg
// concatenation for Template instances). `mr_type_args` are the
// MethodResolution.type_args (already substituted via build_mono_for_template
// — they hold the method-level args).
#[allow(dead_code)]
fn resolve_trait_dispatch_method(
    ctx: &LowerCtx,
    td: &crate::typeck::TraitDispatch,
    mr_type_args: &Vec<RType>,
    span: &crate::span::Span,
) -> Result<u32, Error> {
    // Already substituted at build_mono_for_template; still need to
    // peel any `Ref` wrapper on recv (matches codegen behavior).
    let concrete_recv = match &td.recv_type {
        RType::Ref { inner, .. } => (**inner).clone(),
        other => other.clone(),
    };
    // Never receiver: this call is on a never-reached path (the
    // receiver expression diverged before reaching us — RPIT body
    // returning `!`, etc.). No actual impl exists; wasm's validator
    // accepts subsequent code as polymorphic after the unreachable
    // produced by `panic!`/return/etc., so we hand back any valid
    // import idx (the panic import at slot 0 is always present).
    // The runtime never executes this call.
    if matches!(concrete_recv, RType::Never) {
        return Ok(0);
    }
    let resolution = match solve_impl_with_args(
        &td.trait_path,
        &td.trait_args,
        &concrete_recv,
        ctx.traits,
        0,
    ) {
        Some(r) => r,
        None => return Err(Error {
            file: String::new(),
            message: format!(
                "lower_to_mono: no impl of {} for {} at lowering",
                crate::typeck::place_to_string(&td.trait_path),
                crate::typeck::rtype_to_string(&concrete_recv)
            ),
            span: span.copy(),
        }),
    };
    let cand = match find_trait_impl_method(ctx.funcs, resolution.impl_idx, &td.method_name) {
        Some(c) => c,
        None => return Err(Error {
            file: String::new(),
            message: format!("lower_to_mono: impl missing method `{}`", td.method_name),
            span: span.copy(),
        }),
    };
    match cand {
        MethodCandidate::Direct(i) => Ok(ctx.funcs.entries[i].idx),
        MethodCandidate::Template(i) => {
            // Build the template's concrete arg vector: impl-level slots
            // bound by resolution.subst, then method-level slots from
            // the recorded type_args.
            let tmpl = &ctx.funcs.templates[i];
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
                concrete.push(found.expect("impl-param bound by subst"));
                k += 1;
            }
            let method_param_count = tmpl.type_params.len() - impl_param_count;
            if mr_type_args.len() == method_param_count {
                let mut k = 0;
                while k < method_param_count {
                    concrete.push(mr_type_args[k].clone());
                    k += 1;
                }
            }
            match ctx.mono_table.lookup(i, &concrete) {
                Some(idx) => Ok(idx),
                None => Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: mono_table missing entry for trait-dispatch template {}",
                        i
                    ),
                    span: span.copy(),
                }),
            }
        }
    }
}

// Walk a pattern, allocating BindingIds for each leaf binding (`Binding`
// or `At`) and pushing them onto ctx.scope so the arm body / guard can
// resolve `Var(name)` against them. The leaf binding's type is read
// from `mono_fn.expr_types[pat.id]` (typeck records the pattern type
// for binding leaves).
#[allow(dead_code)]
fn declare_pattern_bindings(ctx: &mut LowerCtx, pat: &AstPattern) -> Result<(), Error> {
    use crate::ast::PatternKind;
    match &pat.kind {
        PatternKind::Binding { name, .. } => {
            let id = pat.id as usize;
            let ty = match ctx.input.expr_types.get(id).and_then(|o| o.as_ref()) {
                Some(t) => t.clone(),
                None => return Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: no expr_type recorded for pattern binding `{}` (pat.id={})",
                        name, pat.id
                    ),
                    span: pat.span.copy(),
                }),
            };
            ctx.declare_binding(name.clone(), ty, BindingOrigin::Pattern(pat.id));
            Ok(())
        }
        PatternKind::At { name, inner, .. } => {
            let id = pat.id as usize;
            let ty = match ctx.input.expr_types.get(id).and_then(|o| o.as_ref()) {
                Some(t) => t.clone(),
                None => return Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: no expr_type for at-pattern `{}`",
                        name
                    ),
                    span: pat.span.copy(),
                }),
            };
            ctx.declare_binding(name.clone(), ty, BindingOrigin::Pattern(pat.id));
            declare_pattern_bindings(ctx, inner)
        }
        PatternKind::Tuple(elems) | PatternKind::VariantTuple { elems, .. } => {
            let mut i = 0;
            while i < elems.len() {
                declare_pattern_bindings(ctx, &elems[i])?;
                i += 1;
            }
            Ok(())
        }
        PatternKind::VariantStruct { fields, .. } => {
            let mut i = 0;
            while i < fields.len() {
                declare_pattern_bindings(ctx, &fields[i].pattern)?;
                i += 1;
            }
            Ok(())
        }
        PatternKind::Ref { inner, .. } => declare_pattern_bindings(ctx, inner),
        PatternKind::Or(alts) => {
            // All alternatives bind the same names; walk only the first.
            if !alts.is_empty() {
                declare_pattern_bindings(ctx, &alts[0])?;
            }
            Ok(())
        }
        PatternKind::Wildcard
        | PatternKind::LitInt(_)
        | PatternKind::LitBool(_)
        | PatternKind::Range { .. } => Ok(()),
    }
}

// Resolve a trait method by recv-type (no trait args). Mirrors
// codegen's solve_impl + find_trait_impl_method + Template-arg-build
// for cases like Iterator::next, Drop::drop, Deref::deref.
#[allow(dead_code)]
fn resolve_trait_method_no_args(
    ctx: &LowerCtx,
    trait_path: &Vec<String>,
    recv_ty: &RType,
    method_name: &str,
    span: &crate::span::Span,
) -> Result<u32, Error> {
    let resolution = match solve_impl(trait_path, recv_ty, ctx.traits, 0) {
        Some(r) => r,
        None => return Err(Error {
            file: String::new(),
            message: format!(
                "lower_to_mono: no impl of {} for {}",
                crate::typeck::place_to_string(trait_path),
                crate::typeck::rtype_to_string(recv_ty)
            ),
            span: span.copy(),
        }),
    };
    let cand = match find_trait_impl_method(ctx.funcs, resolution.impl_idx, method_name) {
        Some(c) => c,
        None => return Err(Error {
            file: String::new(),
            message: format!("lower_to_mono: impl missing method `{}`", method_name),
            span: span.copy(),
        }),
    };
    match cand {
        MethodCandidate::Direct(i) => Ok(ctx.funcs.entries[i].idx),
        MethodCandidate::Template(i) => {
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
                concrete.push(found.expect("impl-param bound by subst"));
                k += 1;
            }
            match ctx.mono_table.lookup(i, &concrete) {
                Some(idx) => Ok(idx),
                None => Err(Error {
                    file: String::new(),
                    message: format!(
                        "lower_to_mono: mono_table missing entry for trait method template {}",
                        i
                    ),
                    span: span.copy(),
                }),
            }
        }
    }
}

// Build a single-segment AstPath suitable for a synth variant pattern
// (e.g. "Some" / "None" / "Ok" / "Err"). Codegen's
// `codegen_variant_pattern` resolves variants by the LAST segment's
// name against the scrutinee's enum type, so a single segment suffices.
#[allow(dead_code)]
fn synth_variant_path(name: &str, span: crate::span::Span) -> crate::ast::Path {
    crate::ast::Path {
        segments: vec![crate::ast::PathSegment {
            name: name.to_string(),
            span: span.copy(),
            lifetime_args: Vec::new(),
            args: Vec::new(),
        }],
        span,
    }
}

// Look up a variant's discriminant by name in an enum.
#[allow(dead_code)]
fn lookup_variant_disc(
    enums: &EnumTable,
    enum_path: &Vec<String>,
    variant_name: &str,
) -> Option<u32> {
    let entry = crate::typeck::enum_lookup(enums, enum_path)?;
    let mut i = 0;
    while i < entry.variants.len() {
        if entry.variants[i].name == variant_name {
            return Some(entry.variants[i].disc);
        }
        i += 1;
    }
    None
}

// Lower `for pat in iter { body }` to:
//   { let __iter = iter;
//     'lbl: loop {
//       match Iterator::next(&mut __iter) {
//         Some(pat) => body,
//         None => break,
//       }
//     }
//   }
#[allow(dead_code)]
fn lower_for(
    ctx: &mut LowerCtx,
    f: &crate::ast::ForLoop,
    expr_ty: &RType,
    span: &crate::span::Span,
) -> Result<MonoExprKind, Error> {
    let iter_lowered = lower_expr(ctx, &f.iter)?;
    let iter_ty = iter_lowered.ty.clone();
    let iter_span = f.iter.span.copy();

    // Resolve Iterator::next's wasm_idx and Item type.
    let iterator_path = vec![
        "std".to_string(),
        "iter".to_string(),
        "Iterator".to_string(),
    ];
    let next_wasm_idx = resolve_trait_method_no_args(
        ctx,
        &iterator_path,
        &iter_ty,
        "next",
        span,
    )?;
    let item_ty = match crate::typeck::find_assoc_binding(
        ctx.traits,
        &iter_ty,
        &iterator_path,
        "Item",
    )
    .into_iter()
    .next()
    {
        Some(t) => t,
        None => return Err(Error {
            file: String::new(),
            message: "lower_to_mono: Iterator::Item not found".to_string(),
            span: span.copy(),
        }),
    };
    let option_path = vec![
        "std".to_string(),
        "option".to_string(),
        "Option".to_string(),
    ];
    let option_item_ty = RType::Enum {
        path: option_path.clone(),
        type_args: vec![item_ty.clone()],
        lifetime_args: Vec::new(),
    };

    // Synth __iter binding.
    let iter_name = ctx.next_synth_name("iter");
    let iter_binding = ctx.declare_binding(
        iter_name.clone(),
        iter_ty.clone(),
        BindingOrigin::Synthesized(iter_name.clone()),
    );
    let iter_let = MonoStmt::Let {
        binding: iter_binding,
        value: iter_lowered,
        span: iter_span.copy(),
    };

    // `&mut __iter` — a Borrow of the local.
    let iter_borrow_ty = RType::Ref {
        inner: Box::new(iter_ty.clone()),
        mutable: true,
        lifetime: crate::typeck::LifetimeRepr::Inferred(0),
    };
    let iter_borrow = MonoExpr {
        kind: MonoExprKind::Borrow {
            place: MonoPlace {
                kind: MonoPlaceKind::Local(iter_binding),
                ty: iter_ty.clone(),
                span: iter_span.copy(),
            },
            mutable: true,
        },
        ty: iter_borrow_ty,
        span: iter_span.copy(),
    };

    // Iterator::next call.
    let next_call = MonoExpr {
        kind: MonoExprKind::MethodCall {
            wasm_idx: next_wasm_idx,
            recv_adjust: ReceiverAdjust::ByRef,
            recv: Box::new(iter_borrow),
            args: Vec::new(),
        },
        ty: option_item_ty.clone(),
        span: iter_span.copy(),
    };

    // Synthesize Some(pat) and None patterns.
    let some_pat = AstPattern {
        kind: crate::ast::PatternKind::VariantTuple {
            path: synth_variant_path("Some", span.copy()),
            elems: vec![f.pattern.clone()],
        },
        span: span.copy(),
        id: 0,
    };
    let none_pat = AstPattern {
        kind: crate::ast::PatternKind::VariantTuple {
            path: synth_variant_path("None", span.copy()),
            elems: Vec::new(),
        },
        span: span.copy(),
        id: 0,
    };

    // Declare the user's pattern bindings BEFORE lowering body so that
    // body's `Var(name)` lookups resolve. Pop after so subsequent code
    // doesn't see them. The synth Some(pat) arm refers to f.pattern by
    // its original AST node; codegen will need a pat.id → BindingId
    // map (Phase 1c) to link the lowered body's `Local(BindingId)`
    // refs back to the wasm locals codegen creates at pattern bind time.
    let scope_mark = ctx.scope.len();
    declare_pattern_bindings(ctx, &f.pattern)?;
    let body_lowered_block = lower_block(ctx, f.body.as_ref())?;
    while ctx.scope.len() > scope_mark {
        ctx.scope.pop();
    }
    let body_span = f.body.span.copy();
    let body_expr = MonoExpr {
        kind: MonoExprKind::Block(Box::new(body_lowered_block)),
        ty: RType::Tuple(Vec::new()),
        span: body_span.copy(),
    };

    let break_expr = MonoExpr {
        kind: MonoExprKind::Break { label: f.label.clone(), value: None },
        ty: RType::Never,
        span: span.copy(),
    };

    let match_expr = MonoExpr {
        kind: MonoExprKind::Match {
            scrutinee: Box::new(next_call),
            arms: vec![
                MonoArm {
                    pattern: some_pat,
                    guard: None,
                    body: body_expr,
                    span: body_span.copy(),
                },
                MonoArm {
                    pattern: none_pat,
                    guard: None,
                    body: break_expr,
                    span: span.copy(),
                },
            ],
        },
        ty: RType::Tuple(Vec::new()),
        span: span.copy(),
    };

    let loop_body = MonoBlock {
        stmts: Vec::new(),
        tail: Some(match_expr),
        span: body_span.copy(),
    };
    let loop_expr = MonoExpr {
        kind: MonoExprKind::Loop {
            label: f.label.clone(),
            body: Box::new(loop_body),
        },
        ty: expr_ty.clone(),
        span: span.copy(),
    };

    // Wrap in a block holding the let __iter = ... and the loop.
    let outer_block = MonoBlock {
        stmts: vec![iter_let],
        tail: Some(loop_expr),
        span: span.copy(),
    };

    Ok(MonoExprKind::Block(Box::new(outer_block)))
}

// Lower `expr?` to:
//   match expr {
//     Ok(v) => v,
//     Err(e) => return Err(e),
//   }
#[allow(dead_code)]
fn lower_try(
    ctx: &mut LowerCtx,
    inner: &Expr,
    expr_ty: &RType,
    question_span: &crate::span::Span,
    span: &crate::span::Span,
) -> Result<MonoExprKind, Error> {
    let scrut = lower_expr(ctx, inner)?;
    let scrut_ty = scrut.ty.clone();
    // scrut_ty must be Result<T, E>; expr_ty is T.
    let (result_path, result_type_args) = match &scrut_ty {
        RType::Enum { path, type_args, .. } => (path.clone(), type_args.clone()),
        _ => return Err(Error {
            file: String::new(),
            message: "lower_to_mono: `?` operator inner type isn't an enum (Result expected)".to_string(),
            span: span.copy(),
        }),
    };
    if result_type_args.len() != 2 {
        return Err(Error {
            file: String::new(),
            message: "lower_to_mono: `?` Result type doesn't have 2 type args".to_string(),
            span: span.copy(),
        });
    }
    let err_ty = result_type_args[1].clone();
    let err_disc = match lookup_variant_disc(ctx.enums, &result_path, "Err") {
        Some(d) => d,
        None => return Err(Error {
            file: String::new(),
            message: "lower_to_mono: Err variant not found".to_string(),
            span: span.copy(),
        }),
    };

    // Synth bindings for the Ok/Err arms — these are pattern bindings
    // declared by the synth Ok(v) / Err(e) patterns. Since the patterns
    // are AstPatterns (not BindingId-resolved), we need to push the
    // synth-name bindings onto scope manually for the arm bodies.
    let v_name = ctx.next_synth_name("ok_val");
    let e_name = ctx.next_synth_name("err_val");

    let ok_pat = AstPattern {
        kind: crate::ast::PatternKind::VariantTuple {
            path: synth_variant_path("Ok", question_span.copy()),
            elems: vec![AstPattern {
                kind: crate::ast::PatternKind::Binding {
                    name: v_name.clone(),
                    name_span: question_span.copy(),
                    by_ref: false,
                    mutable: false,
                },
                span: question_span.copy(),
                id: 0,
            }],
        },
        span: question_span.copy(),
        id: 0,
    };
    let err_pat = AstPattern {
        kind: crate::ast::PatternKind::VariantTuple {
            path: synth_variant_path("Err", question_span.copy()),
            elems: vec![AstPattern {
                kind: crate::ast::PatternKind::Binding {
                    name: e_name.clone(),
                    name_span: question_span.copy(),
                    by_ref: false,
                    mutable: false,
                },
                span: question_span.copy(),
                id: 0,
            }],
        },
        span: question_span.copy(),
        id: 0,
    };

    // Allocate BindingIds for v and e. Push them onto scope; arm
    // bodies resolve `Var(v_name)` / `Var(e_name)` via these.
    let v_binding = ctx.declare_binding(
        v_name.clone(),
        expr_ty.clone(),
        BindingOrigin::Synthesized(v_name.clone()),
    );
    let ok_body = MonoExpr {
        kind: MonoExprKind::Local(v_binding, u32::MAX),
        ty: expr_ty.clone(),
        span: question_span.copy(),
    };
    // Pop v from scope before declaring e.
    ctx.scope.pop();

    let e_binding = ctx.declare_binding(
        e_name.clone(),
        err_ty.clone(),
        BindingOrigin::Synthesized(e_name.clone()),
    );

    // Build `return Err(e)`. We need a function-return-typed
    // VariantConstruct whose enum_path is the function's return type's
    // enum path. The function's return type IS the same Result enum
    // (by typeck's ?-rules), so we reuse result_path. The function's
    // return type's Ok arg might differ from this expression's; pull
    // from mono_fn.return_type.
    let fn_return_ty = match &ctx.input.return_type {
        Some(t) => t.clone(),
        None => return Err(Error {
            file: String::new(),
            message: "lower_to_mono: `?` in unit-returning fn".to_string(),
            span: span.copy(),
        }),
    };
    let (fn_result_path, fn_result_type_args) = match &fn_return_ty {
        RType::Enum { path, type_args, .. } => (path.clone(), type_args.clone()),
        _ => return Err(Error {
            file: String::new(),
            message: "lower_to_mono: `?` outer fn return isn't an enum".to_string(),
            span: span.copy(),
        }),
    };
    let return_err_disc = match lookup_variant_disc(ctx.enums, &fn_result_path, "Err") {
        Some(d) => d,
        None => return Err(Error {
            file: String::new(),
            message: "lower_to_mono: Err variant not found in fn return type".to_string(),
            span: span.copy(),
        }),
    };
    let _ = err_disc; // both should equal; using fn_return's for the constructor

    let err_value = MonoExpr {
        kind: MonoExprKind::Local(e_binding, u32::MAX),
        ty: err_ty.clone(),
        span: question_span.copy(),
    };
    let err_construct = MonoExpr {
        kind: MonoExprKind::VariantConstruct {
            enum_path: fn_result_path,
            type_args: fn_result_type_args,
            disc: return_err_disc,
            payload: vec![err_value],
        },
        ty: fn_return_ty,
        span: question_span.copy(),
    };
    let err_body = MonoExpr {
        kind: MonoExprKind::Return {
            value: Some(Box::new(err_construct)),
        },
        ty: RType::Never,
        span: question_span.copy(),
    };
    // Pop e from scope.
    ctx.scope.pop();

    Ok(MonoExprKind::Match {
        scrutinee: Box::new(scrut),
        arms: vec![
            MonoArm {
                pattern: ok_pat,
                guard: None,
                body: ok_body,
                span: question_span.copy(),
            },
            MonoArm {
                pattern: err_pat,
                guard: None,
                body: err_body,
                span: question_span.copy(),
            },
        ],
    })
}

// Walk a place's auto-deref chain until the place's type has the named
// field directly on a struct. Each deref level inserts an explicit
// `MonoPlaceKind::Deref` node — codegen never has to infer auto-deref.
//
// Handles:
//   - `Ref`/`RawPtr`: wrap in `Deref { inner: PlaceLoad(base) }`, use pointee.
//   - struct with `Deref` impl: synth `Deref::deref(&base)` MethodCall,
//     wrap in `Deref { inner: <call> }`, use Target type.
//
// Stops when the current type is a struct with the field. Errors if no
// auto-deref step makes progress.
#[allow(dead_code)]
fn auto_deref_until_field(
    ctx: &mut LowerCtx,
    base: MonoPlace,
    field_name: &str,
    span: &crate::span::Span,
) -> Result<MonoPlace, Error> {
    let mut current = base;
    loop {
        if struct_has_field(ctx, &current.ty, field_name) {
            return Ok(current);
        }
        current = deref_one_level(ctx, current, span)?;
    }
}

#[allow(dead_code)]
fn auto_deref_until_tuple(
    ctx: &mut LowerCtx,
    base: MonoPlace,
    index: u32,
    span: &crate::span::Span,
) -> Result<MonoPlace, Error> {
    let mut current = base;
    loop {
        if let RType::Tuple(elems) = &current.ty {
            if (index as usize) < elems.len() {
                return Ok(current);
            }
        }
        current = deref_one_level(ctx, current, span)?;
    }
}

// Apply one level of auto-deref to `place`, inserting an explicit
// MonoPlaceKind::Deref. For Ref/RawPtr-typed places: PlaceLoad of base
// is the inner expression (the binding's value IS the address). For
// struct-typed places: synth Deref::deref(&base) MethodCall.
#[allow(dead_code)]
fn deref_one_level(
    ctx: &mut LowerCtx,
    place: MonoPlace,
    span: &crate::span::Span,
) -> Result<MonoPlace, Error> {
    let place_span = place.span.copy();
    match &place.ty {
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } => {
            let pointee_ty = (**inner).clone();
            let ty_for_load = place.ty.clone();
            // Wrap the place as a PlaceLoad expression — value IS the
            // pointer for Ref/RawPtr types.
            let inner_expr = MonoExpr {
                kind: MonoExprKind::PlaceLoad(place),
                ty: ty_for_load,
                span: place_span.copy(),
            };
            Ok(MonoPlace {
                kind: MonoPlaceKind::Deref { inner: Box::new(inner_expr) },
                ty: pointee_ty,
                span: place_span,
            })
        }
        RType::Struct { .. } => {
            // Smart-pointer deref via Deref trait.
            let ty_for_load = place.ty.clone();
            let value_expr = MonoExpr {
                kind: MonoExprKind::PlaceLoad(place),
                ty: ty_for_load,
                span: place_span.copy(),
            };
            let (call_expr, target_ty) = synth_deref_call(ctx, value_expr, span)?;
            Ok(MonoPlace {
                kind: MonoPlaceKind::Deref { inner: Box::new(call_expr) },
                ty: target_ty,
                span: place_span,
            })
        }
        other => Err(Error {
            file: String::new(),
            message: format!(
                "lower_to_mono: cannot auto-deref non-pointer non-struct {}",
                crate::typeck::rtype_to_string(other)
            ),
            span: span.copy(),
        }),
    }
}

// Synthesize a `Deref::deref(&recv)` MethodCall MonoExpr. Returns the
// call expression and its `Target` type. Used for smart-pointer
// auto-deref in field/tuple-index access.
#[allow(dead_code)]
// Build a synthesized `<Index|IndexMut>::index{,_mut}(&base, idx)`
// MethodCall MonoExpr. Used by both place- and value-context
// `arr[i]` lowering to desugar Index away — codegen never sees an
// Index node, only standard MethodCall + Deref-of-MethodCall shapes.
//
// `output_ty` is the indexed element type (the trait's `Output`).
// The method's return type is `&Output` (or `&mut Output` for the
// mutable variant), which is what this function's MonoExpr.ty is set
// to. The caller wraps the result in `MonoPlace::Deref` to access the
// element.
fn synth_index_call(
    ctx: &mut LowerCtx,
    base: &Expr,
    index: &Expr,
    mutable: bool,
    output_ty: &RType,
    span: &crate::span::Span,
) -> Result<MonoExpr, Error> {
    let base_lowered = lower_expr(ctx, base)?;
    let index_lowered = lower_expr(ctx, index)?;
    let base_ty = base_lowered.ty.clone();
    let lookup_recv = match &base_ty {
        RType::Ref { inner, .. } => (**inner).clone(),
        _ => base_ty,
    };
    let idx_ty = index_lowered.ty.clone();
    let (trait_path, method_name) = if mutable {
        (
            vec!["std".to_string(), "ops".to_string(), "IndexMut".to_string()],
            "index_mut",
        )
    } else {
        (
            vec!["std".to_string(), "ops".to_string(), "Index".to_string()],
            "index",
        )
    };
    let wasm_idx = resolve_index_method(
        ctx,
        &trait_path,
        &vec![idx_ty],
        &lookup_recv,
        method_name,
    )?;
    // recv_adjust: if base is already a `&T`/`&mut T`, pass through
    // (`ByRef`) — codegen pushes the full flat value (e.g. slice fat
    // refs are 2 i32s; a single autoref'd address would lose the len).
    // If base is owned `T`, autoref via BorrowImm/BorrowMut so codegen
    // takes its address.
    let recv_adjust = match &base_lowered.ty {
        RType::Ref { .. } => ReceiverAdjust::ByRef,
        _ => {
            if mutable {
                ReceiverAdjust::BorrowMut
            } else {
                ReceiverAdjust::BorrowImm
            }
        }
    };
    let ref_ty = RType::Ref {
        inner: Box::new(output_ty.clone()),
        mutable,
        lifetime: crate::typeck::LifetimeRepr::Inferred(0),
    };
    Ok(MonoExpr {
        kind: MonoExprKind::MethodCall {
            wasm_idx,
            recv_adjust,
            recv: Box::new(base_lowered),
            args: vec![index_lowered],
        },
        ty: ref_ty,
        span: span.copy(),
    })
}

fn synth_deref_call(
    ctx: &LowerCtx,
    recv: MonoExpr,
    span: &crate::span::Span,
) -> Result<(MonoExpr, RType), Error> {
    let deref_path = vec![
        "std".to_string(),
        "ops".to_string(),
        "Deref".to_string(),
    ];
    let target_ty = match crate::typeck::find_assoc_binding(
        ctx.traits,
        &recv.ty,
        &deref_path,
        "Target",
    )
    .into_iter()
    .next()
    {
        Some(t) => t,
        None => return Err(Error {
            file: String::new(),
            message: format!(
                "lower_to_mono: no Deref impl for {}",
                crate::typeck::rtype_to_string(&recv.ty)
            ),
            span: span.copy(),
        }),
    };
    let wasm_idx = resolve_trait_method_no_args(
        ctx,
        &deref_path,
        &recv.ty,
        "deref",
        span,
    )?;
    let target_ref_ty = RType::Ref {
        inner: Box::new(target_ty.clone()),
        mutable: false,
        lifetime: crate::typeck::LifetimeRepr::Inferred(0),
    };
    let call = MonoExpr {
        kind: MonoExprKind::MethodCall {
            wasm_idx,
            recv_adjust: ReceiverAdjust::BorrowImm,
            recv: Box::new(recv),
            args: Vec::new(),
        },
        ty: target_ref_ty,
        span: span.copy(),
    };
    Ok((call, target_ty))
}

#[allow(dead_code)]
fn struct_has_field(ctx: &LowerCtx, ty: &RType, field_name: &str) -> bool {
    match ty {
        RType::Struct { path, .. } => {
            match crate::typeck::struct_lookup(ctx.structs, path) {
                Some(entry) => entry.fields.iter().any(|f| f.name == *field_name),
                None => false,
            }
        }
        _ => false,
    }
}
