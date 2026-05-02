// AST → CFG converter. Lowers a typeck'd `Function` body into a `Cfg`.
//
// Each compound expression evaluates left-to-right; intermediate values
// land in compiler-introduced temporary `LocalDecl`s. Control-flow
// expressions (if/match/if-let, future while) split blocks and merge at
// a successor.

use crate::ast::{
    self, AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, IfLetExpr, LetStmt,
    MatchExpr, MethodCall, Pattern, PatternKind, Stmt, StructLit,
};
use super::cfg::{
    BasicBlock, BlockId, CallTarget, Cfg, CfgStmt, CfgStmtKind, LocalDecl, LocalId, Operand,
    OperandKind, Place, Projection, RegionId, Rvalue, Terminator, VariantFields,
};
use crate::span::Span;
use crate::typeck::{
    CallResolution, EnumTable, EnumVariantEntry, FuncTable, IntKind, LifetimeRepr,
    MethodResolution, RType, ReceiverAdjust, StructTable, TraitTable, VariantPayloadResolved,
    is_copy_with_bounds, substitute_rtype,
};

// Per-binding-name → LocalId map; lookup walks the stack of scopes from
// innermost out.
struct Scope {
    names: Vec<(String, LocalId)>,
}

pub struct CfgBuildCtx<'a> {
    pub structs: &'a StructTable,
    pub enums: &'a EnumTable,
    pub traits: &'a TraitTable,
    pub funcs: &'a FuncTable,
    pub expr_types: &'a Vec<Option<RType>>,
    pub method_resolutions: &'a Vec<Option<MethodResolution>>,
    pub call_resolutions: &'a Vec<Option<CallResolution>>,
    pub type_params: &'a Vec<String>,
    pub type_param_bounds: &'a Vec<Vec<Vec<String>>>,
    // Resolved parameter types (in order). Length = func.params.len().
    pub param_types: &'a Vec<RType>,
    // Resolved return type (`()` if absent in source).
    pub return_type: &'a RType,
}

// Active-loop bookkeeping for break/continue.
struct LoopFrame {
    label: Option<String>,
    // Block to jump to when continuing the loop (the cond-eval block).
    continue_target: BlockId,
    // Block to jump to when breaking out of the loop.
    break_target: BlockId,
}

struct Builder<'a> {
    ctx: &'a CfgBuildCtx<'a>,
    blocks: Vec<BasicBlock>,
    locals: Vec<LocalDecl>,
    region_count: u32,
    current_block: BlockId,
    scopes: Vec<Scope>,
    return_local: Option<LocalId>,
    param_count: u32,
    loops: Vec<LoopFrame>,
}

pub fn build(func: &Function, ctx: &CfgBuildCtx) -> Cfg {
    let mut b = Builder {
        ctx,
        blocks: Vec::new(),
        locals: Vec::new(),
        region_count: 0,
        current_block: 0,
        scopes: Vec::new(),
        return_local: None,
        param_count: 0,
        loops: Vec::new(),
    };
    let entry = b.new_block();
    b.current_block = entry;

    // Reserve return local 0 if non-unit return.
    if !is_unit(ctx.return_type) {
        let id = b.alloc_local(
            None,
            ctx.return_type.clone(),
            func.name_span.copy(),
            true,
            false,
        );
        b.return_local = Some(id);
    }

    b.push_scope();
    // Allocate parameter locals.
    let mut i = 0;
    while i < func.params.len() {
        let p = &func.params[i];
        let rt = ctx.param_types[i].clone();
        let id = b.alloc_local(Some(p.name.clone()), rt, p.name_span.copy(), false, false);
        b.bind_name(&p.name, id);
        b.param_count += 1;
        i += 1;
    }

    // Lower the body. The function's tail expression (if any) feeds the
    // return local; otherwise the body is unit-returning and we just
    // run statements.
    b.lower_block(&func.body, b.return_local.map(local_place));
    // Final terminator.
    b.set_terminator(Terminator::Return);
    b.pop_scope();

    Cfg {
        blocks: b.blocks,
        locals: b.locals,
        entry,
        region_count: b.region_count,
        return_local: b.return_local,
        param_count: b.param_count,
    }
}

impl<'a> Builder<'a> {
    fn new_block(&mut self) -> BlockId {
        let id = self.blocks.len() as BlockId;
        self.blocks.push(BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    fn push_stmt(&mut self, kind: CfgStmtKind, span: Span) {
        self.blocks[self.current_block as usize]
            .stmts
            .push(CfgStmt { kind, span });
    }

    fn set_terminator(&mut self, t: Terminator) {
        self.blocks[self.current_block as usize].terminator = t;
    }

    fn alloc_local(
        &mut self,
        name: Option<String>,
        ty: RType,
        span: Span,
        mutable: bool,
        is_temp: bool,
    ) -> LocalId {
        let id = self.locals.len() as LocalId;
        self.locals.push(LocalDecl {
            name,
            ty,
            span,
            mutable,
            is_temp,
        });
        id
    }

    fn alloc_temp(&mut self, ty: RType, span: Span) -> LocalId {
        self.alloc_local(None, ty, span, true, true)
    }

    fn fresh_region(&mut self) -> RegionId {
        let r = self.region_count;
        self.region_count += 1;
        r
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope { names: Vec::new() });
    }

    fn pop_scope(&mut self) {
        if let Some(s) = self.scopes.pop() {
            // Emit StorageDead in declaration-reverse order at scope exit.
            let mut i = s.names.len();
            while i > 0 {
                i -= 1;
                let (_name, local) = s.names[i].clone();
                self.push_stmt(
                    CfgStmtKind::StorageDead(local),
                    self.locals[local as usize].span.copy(),
                );
            }
        }
    }

    fn bind_name(&mut self, name: &str, local: LocalId) {
        let s = self.scopes.last_mut().expect("scope must be active");
        s.names.push((name.to_string(), local));
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        let mut i = self.scopes.len();
        while i > 0 {
            i -= 1;
            let s = &self.scopes[i];
            let mut j = s.names.len();
            while j > 0 {
                j -= 1;
                if s.names[j].0 == name {
                    return Some(s.names[j].1);
                }
            }
        }
        None
    }

    // Lower a block. If `target` is Some, the block's tail expression
    // is assigned to it; otherwise the tail (if any) is evaluated and
    // discarded.
    fn lower_block(&mut self, block: &Block, target: Option<Place>) {
        self.push_scope();
        let mut i = 0;
        while i < block.stmts.len() {
            self.lower_stmt(&block.stmts[i]);
            i += 1;
        }
        if let Some(tail) = &block.tail {
            if let Some(t) = target.clone() {
                self.lower_expr_into(tail, t);
            } else {
                let _ = self.lower_expr_operand(tail);
            }
        }
        self.pop_scope();
    }

    fn lower_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(let_stmt) => self.lower_let(let_stmt),
            Stmt::Assign(assign) => self.lower_assign(assign),
            Stmt::Expr(expr) => {
                let _ = self.lower_expr_operand(expr);
            }
            Stmt::Use(_) => {}
        }
    }

    fn lower_let(&mut self, ls: &LetStmt) {
        let ty = self.expr_type(ls.value.id);
        // Simple-binding fast path: `let x = e;` / `let mut x = e;`.
        // Tuple/struct destructure and let-else go through the
        // pattern-binding path below.
        if let Some((name, mutable, name_span)) = crate::ast::let_simple_binding(ls) {
            let id = self.alloc_local(
                Some(name.to_string()),
                ty.clone(),
                name_span.copy(),
                mutable,
                false,
            );
            self.push_stmt(CfgStmtKind::StorageLive(id), name_span.copy());
            let place = local_place(id);
            self.lower_expr_into(&ls.value, place);
            self.bind_name(name, id);
            return;
        }
        // General pattern: lower the value into a temp, then walk
        // the pattern to bind sub-places. (let-else, tuple destructure,
        // wildcard `let _ = e;`, etc.) Note: we rely on typeck having
        // rejected refutable patterns without a let-else.
        let scrut = self.alloc_temp(ty.clone(), ls.value.span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(scrut), ls.value.span.copy());
        self.lower_expr_into(&ls.value, local_place(scrut));
        let scrut_place = local_place(scrut);
        // For let-else: emit a pattern test; on no-match, lower the
        // else-block (which must diverge per typeck) so we never fall
        // through.
        if let Some(else_block) = &ls.else_block {
            let then_block = self.new_block();
            let else_target = self.new_block();
            self.lower_pattern_test(
                &ls.pattern,
                &scrut_place,
                &ty,
                then_block,
                else_target,
            );
            self.current_block = else_target;
            self.lower_block(else_block.as_ref(), None);
            // Else block diverges (typeck-enforced) — but emit a
            // safety terminator in case lowering didn't already.
            self.set_terminator(super::cfg::Terminator::Unreachable);
            self.current_block = then_block;
        }
        let bindings = self.collect_pattern_bindings(&ls.pattern, &scrut_place, &ty);
        self.materialize_bindings(&bindings);
    }

    fn lower_assign(&mut self, a: &AssignStmt) {
        let place = self.lower_expr_place(&a.lhs);
        self.lower_expr_into(&a.rhs, place);
    }

    // Lower `expr` and store its value into `dest`.
    fn lower_expr_into(&mut self, expr: &Expr, dest: Place) {
        match &expr.kind {
            ExprKind::If(if_expr) => {
                self.lower_if_into(if_expr, expr.span.copy(), Some(dest));
            }
            ExprKind::Match(m) => {
                self.lower_match_into(m, Some(dest));
            }
            ExprKind::IfLet(il) => {
                self.lower_if_let_into(il, Some(dest));
            }
            ExprKind::While(w) => {
                self.lower_while(w, expr.span.copy());
                // The while expression evaluates to (); init dest with
                // unit (no-op for unit dest, but explicit for sanity).
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue: Rvalue::Use(Operand {
                            kind: OperandKind::ConstUnit,
                            span: expr.span.copy(),
                            node_id: Some(expr.id),
                        }),
                    },
                    expr.span.copy(),
                );
            }
            ExprKind::For(f) => {
                self.lower_for(f, expr.span.copy());
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue: Rvalue::Use(Operand {
                            kind: OperandKind::ConstUnit,
                            span: expr.span.copy(),
                            node_id: Some(expr.id),
                        }),
                    },
                    expr.span.copy(),
                );
            }
            ExprKind::Break { label, label_span: _ } => {
                self.lower_break(label, expr.span.copy());
            }
            ExprKind::Continue { label, label_span: _ } => {
                self.lower_continue(label, expr.span.copy());
            }
            ExprKind::Return { value } => {
                self.lower_return(value.as_deref(), expr.span.copy());
            }
            ExprKind::Try { inner, .. } => {
                // The `?` operator's "happy path" extracts the Ok payload
                // into `dest`; the error path returns the function early.
                // For borrowck purposes we model it as: read inner once
                // (non-trivially — the Result is consumed), then assign
                // the Ok payload to `dest`. The Err-return diverges; we
                // don't need to thread it through borrowck since the
                // function ends at that point.
                let _ = self.lower_expr_operand(inner);
                // Synthesize an Operand-of-Ok-payload by reading the
                // inner's place — since we already lowered the inner
                // for its side effects, re-treat its place as the
                // source. Conservative.
                let rvalue = self.lower_expr_rvalue(inner);
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue,
                    },
                    expr.span.copy(),
                );
            }
            ExprKind::Block(b) => {
                self.lower_block(b.as_ref(), Some(dest));
            }
            ExprKind::Unsafe(b) => {
                self.lower_block(b.as_ref(), Some(dest));
            }
            // For all other expressions, lower to an rvalue and assign.
            _ => {
                let rvalue = self.lower_expr_rvalue(expr);
                self.push_stmt(
                    CfgStmtKind::Assign {
                        place: dest,
                        rvalue,
                    },
                    expr.span.copy(),
                );
            }
        }
    }

    // Lower an expression to an Operand (move/copy of a place, or a
    // constant). Compound exprs go through a temporary.
    fn lower_expr_operand(&mut self, expr: &Expr) -> Operand {
        let span = expr.span.copy();
        let nid = Some(expr.id);
        match &expr.kind {
            ExprKind::IntLit(n) => Operand { kind: OperandKind::ConstInt(*n), span, node_id: nid },
            // CFG-level borrowck doesn't do arithmetic; the sign of
            // a literal carries no move/borrow semantics, so we treat
            // `NegIntLit(n)` as just another constant operand.
            ExprKind::NegIntLit(n) => Operand { kind: OperandKind::ConstInt(*n), span, node_id: nid },
            ExprKind::StrLit(s) => Operand { kind: OperandKind::ConstStr(s.clone()), span, node_id: nid },
            ExprKind::BoolLit(b) => Operand { kind: OperandKind::ConstBool(*b), span, node_id: nid },
            // Char literals reduce to a 4-byte i32 codepoint — for
            // CFG-level borrowck purposes the same as an integer
            // constant.
            ExprKind::CharLit(c) => Operand { kind: OperandKind::ConstInt(*c as u64), span, node_id: nid },
            ExprKind::Tuple(elems) if elems.is_empty() => {
                Operand { kind: OperandKind::ConstUnit, span, node_id: nid }
            }
            ExprKind::Var(name) => {
                let local = self.lookup(name).expect("typeck verified");
                let place = local_place(local);
                let ty = self.locals[local as usize].ty.clone();
                self.materialize_if_move(&place, ty, span, nid)
            }
            ExprKind::FieldAccess(_)
            | ExprKind::TupleIndex { .. }
            | ExprKind::Deref(_) => {
                let place = self.lower_expr_place(expr);
                let ty = self.expr_type(expr.id);
                self.materialize_if_move(&place, ty, span, nid)
            }
            _ => {
                // Other expressions need a temporary.
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(
                    CfgStmtKind::StorageLive(temp),
                    expr.span.copy(),
                );
                self.lower_expr_into(expr, local_place(temp));
                // Reading from a temp isn't a source-level binding
                // move, so don't pin a node_id (drop-flag synthesis
                // would never look at temps anyway).
                self.move_or_copy(&local_place(temp), ty, span, None)
            }
        }
    }

    fn move_or_copy(
        &self,
        place: &Place,
        ty: RType,
        span: Span,
        node_id: Option<ast::NodeId>,
    ) -> Operand {
        let copy = is_copy_with_bounds(
            &ty,
            self.ctx.traits,
            self.ctx.type_params,
            self.ctx.type_param_bounds,
        );
        let kind = if copy {
            OperandKind::Copy(place.clone())
        } else {
            OperandKind::Move(place.clone())
        };
        Operand { kind, span, node_id }
    }

    // For a place-shaped operand: if the operand would be a Move
    // (non-Copy), emit `temp = move place` here so the move-effect lands
    // in the CFG at source-evaluation order — not deferred to the outer
    // statement that consumes the operand. Without this, e.g.
    // `f(o.x, g(&o))` would record both args' effects in the outer
    // Call's stmt, with `&o` (lowered through nested temps) processed
    // first and `move o.x` processed last; subsequent borrows that
    // should fail because of the move would spuriously succeed.
    //
    // Copy reads need no materialization (they have no move-effect, and
    // reading a Copy place again later is fine).
    fn materialize_if_move(
        &mut self,
        place: &Place,
        ty: RType,
        span: Span,
        node_id: Option<ast::NodeId>,
    ) -> Operand {
        let copy = is_copy_with_bounds(
            &ty,
            self.ctx.traits,
            self.ctx.type_params,
            self.ctx.type_param_bounds,
        );
        // Implicit function/builtin-arg reborrow for `&mut T`: same
        // justification as method-receiver reborrow (`lower_recv_reborrow`).
        // The call scope-bounds the borrow; after the call, the original
        // `&mut T` binding is usable again. Without this, calling two
        // builtins/funcs with the same `&mut self` arg in a method body
        // (e.g. `IndexMut for str`'s body using `¤str_len(self)` then
        // `¤str_as_mut_bytes(self)`) errors with "self already moved".
        // Sound because pocket-rust treats the reborrow as exclusive
        // for the duration of the call, just like Rust does.
        let mut_ref_reborrow = matches!(&ty, RType::Ref { mutable: true, .. });
        if copy || mut_ref_reborrow {
            return Operand {
                kind: OperandKind::Copy(place.clone()),
                span,
                node_id,
            };
        }
        let temp = self.alloc_temp(ty.clone(), span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(temp), span.copy());
        let move_op = Operand {
            kind: OperandKind::Move(place.clone()),
            span: span.copy(),
            node_id,
        };
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(temp),
                rvalue: Rvalue::Use(move_op),
            },
            span.copy(),
        );
        Operand {
            kind: OperandKind::Move(local_place(temp)),
            span,
            node_id: None,
        }
    }

    // Lower an expression to a Place (used as borrow target or
    // assignment LHS). Only place-shaped expressions are valid here:
    // Var, FieldAccess, TupleIndex, Deref.
    fn lower_expr_place(&mut self, expr: &Expr) -> Place {
        match &expr.kind {
            ExprKind::Var(name) => {
                let local = self.lookup(name).expect("typeck verified");
                local_place(local)
            }
            ExprKind::FieldAccess(fa) => {
                let mut p = self.lower_expr_place(&fa.base);
                p.projections.push(Projection::Field(fa.field.clone()));
                p
            }
            ExprKind::TupleIndex { base, index, .. } => {
                let mut p = self.lower_expr_place(base);
                p.projections.push(Projection::TupleIndex(*index));
                p
            }
            ExprKind::Deref(inner) => {
                // For `*expr`, we need `expr` to become an addressable
                // place but we shouldn't move it — the ref is just
                // being used to compute the deref address. Treat as
                // an implicit reborrow: when `inner` is itself a place
                // expression (Var/FieldAccess/Deref/etc.) we read its
                // place directly and append `Projection::Deref`,
                // without recording any move on the ref. This is what
                // lets `*self = ¤u8_add(*self, other);` typecheck —
                // both `*self` reads use `self` without consuming it.
                // Non-place inners (e.g. `*foo()`) still go through
                // operand materialization.
                if matches!(
                    inner.kind,
                    ExprKind::Var(_)
                        | ExprKind::FieldAccess(_)
                        | ExprKind::TupleIndex { .. }
                        | ExprKind::Deref(_)
                ) {
                    let mut p = self.lower_expr_place(inner);
                    p.projections.push(Projection::Deref);
                    p
                } else {
                    let op = self.lower_expr_operand(inner);
                    match op.kind {
                        OperandKind::Move(p) | OperandKind::Copy(p) => {
                            let mut p = p;
                            p.projections.push(Projection::Deref);
                            p
                        }
                        _ => unreachable!("typeck rejects deref of constant"),
                    }
                }
            }
            // Non-place expression appearing where a place is needed
            // (e.g., `f().field`): materialize into a fresh temp and
            // return the temp's place.
            _ => {
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_expr_into(expr, local_place(temp));
                local_place(temp)
            }
        }
    }

    // Lower an expression to an Rvalue (suitable for the RHS of
    // Assign). Place-only expressions return Use(Operand).
    fn lower_expr_rvalue(&mut self, expr: &Expr) -> Rvalue {
        let span = expr.span.copy();
        let nid = Some(expr.id);
        match &expr.kind {
            ExprKind::IntLit(n) => Rvalue::Use(Operand {
                kind: OperandKind::ConstInt(*n),
                span,
                node_id: nid,
            }),
            ExprKind::NegIntLit(n) => Rvalue::Use(Operand {
                kind: OperandKind::ConstInt(*n),
                span,
                node_id: nid,
            }),
            ExprKind::StrLit(s) => Rvalue::Use(Operand {
                kind: OperandKind::ConstStr(s.clone()),
                span,
                node_id: nid,
            }),
            ExprKind::BoolLit(b) => Rvalue::Use(Operand {
                kind: OperandKind::ConstBool(*b),
                span,
                node_id: nid,
            }),
            ExprKind::CharLit(c) => Rvalue::Use(Operand {
                kind: OperandKind::ConstInt(*c as u64),
                span,
                node_id: nid,
            }),
            ExprKind::Tuple(elems) if elems.is_empty() => Rvalue::Use(Operand {
                kind: OperandKind::ConstUnit,
                span,
                node_id: nid,
            }),
            ExprKind::Var(_)
            | ExprKind::FieldAccess(_)
            | ExprKind::TupleIndex { .. }
            | ExprKind::Deref(_) => {
                let op = self.lower_expr_operand(expr);
                Rvalue::Use(op)
            }
            ExprKind::Borrow { inner, mutable } => {
                let place = self.lower_expr_place(inner);
                let region = self.fresh_region();
                Rvalue::Borrow {
                    mutable: *mutable,
                    place,
                    region,
                }
            }
            ExprKind::Cast { inner, .. } => {
                let source = self.lower_expr_operand(inner);
                let target_ty = self.expr_type(expr.id);
                Rvalue::Cast { source, target_ty }
            }
            ExprKind::Tuple(elems) => {
                let mut ops: Vec<Operand> = Vec::new();
                let mut i = 0;
                while i < elems.len() {
                    ops.push(self.lower_expr_operand(&elems[i]));
                    i += 1;
                }
                Rvalue::Tuple(ops)
            }
            ExprKind::Builtin { name, args, .. } => {
                let mut ops: Vec<Operand> = Vec::new();
                let mut i = 0;
                while i < args.len() {
                    ops.push(self.lower_expr_operand(&args[i]));
                    i += 1;
                }
                Rvalue::Builtin {
                    name: name.clone(),
                    args: ops,
                }
            }
            ExprKind::Call(c) => self.lower_call(c, expr.id),
            ExprKind::MethodCall(mc) => self.lower_method_call(mc, expr.id),
            ExprKind::StructLit(lit) => self.lower_struct_lit(lit, expr.id),
            ExprKind::If(_) | ExprKind::Block(_) | ExprKind::Unsafe(_) => {
                // Should be handled by lower_expr_into; this path is for
                // places where rvalue is required (e.g., as a sub-rvalue
                // of an assign). For now, materialize through a temp.
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_expr_into(expr, local_place(temp));
                Rvalue::Use(self.move_or_copy(&local_place(temp), ty, span, None))
            }
            ExprKind::Match(m) => {
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_match_into(m, Some(local_place(temp)));
                Rvalue::Use(self.move_or_copy(&local_place(temp), ty, span, None))
            }
            ExprKind::IfLet(il) => {
                let ty = self.expr_type(expr.id);
                let temp = self.alloc_temp(ty.clone(), expr.span.copy());
                self.push_stmt(CfgStmtKind::StorageLive(temp), expr.span.copy());
                self.lower_if_let_into(il, Some(local_place(temp)));
                Rvalue::Use(self.move_or_copy(&local_place(temp), ty, span, None))
            }
            ExprKind::While(w) => {
                self.lower_while(w, expr.span.copy());
                // While expressions evaluate to (). Return a unit
                // operand wrapped as Use.
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::For(f) => {
                self.lower_for(f, expr.span.copy());
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Break { label, label_span: _ } => {
                self.lower_break(label, expr.span.copy());
                // Return value won't be reached at runtime; produce a
                // unit so the rvalue typing flows.
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Continue { label, label_span: _ } => {
                self.lower_continue(label, expr.span.copy());
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Return { value } => {
                self.lower_return(value.as_deref(), expr.span.copy());
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::Try { inner, .. } => {
                // Same modeling as the assign-position case: read the
                // inner once. Borrowck doesn't need to differentiate
                // the Ok/Err split — typeck has verified the shapes.
                self.lower_expr_rvalue(inner)
            }
            ExprKind::Index { base, index, .. } => {
                // Model `arr[idx]` as `Index::index(&arr, idx)` for
                // borrowck purposes — base is *borrowed*, not moved,
                // so subsequent uses of base remain valid.
                let _recv = self.synth_borrow(base, false);
                let _ = self.lower_expr_operand(index);
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
            ExprKind::MacroCall { args, .. } => {
                // panic!(msg) — read the message arg; the call
                // diverges, so subsequent code in the same block is
                // unreachable for borrowck.
                let mut i = 0;
                while i < args.len() {
                    let _ = self.lower_expr_operand(&args[i]);
                    i += 1;
                }
                self.set_terminator(Terminator::Unreachable);
                let unreachable = self.new_block();
                self.current_block = unreachable;
                Rvalue::Use(Operand {
                    kind: OperandKind::ConstUnit,
                    span,
                    node_id: Some(expr.id),
                })
            }
        }
    }

    fn lower_call(&mut self, c: &Call, node_id: ast::NodeId) -> Rvalue {
        let mut args: Vec<Operand> = Vec::new();
        let mut i = 0;
        while i < c.args.len() {
            args.push(self.lower_expr_operand(&c.args[i]));
            i += 1;
        }
        let resolution = self.ctx.call_resolutions[node_id as usize].as_ref();
        let callee = match resolution {
            Some(CallResolution::Direct(idx)) => {
                CallTarget::Path(self.ctx.funcs.entries[*idx].path.clone())
            }
            Some(CallResolution::Generic { template_idx, .. }) => {
                CallTarget::Path(self.ctx.funcs.templates[*template_idx].path.clone())
            }
            Some(CallResolution::Variant {
                enum_path,
                disc,
                type_args,
            }) => {
                return Rvalue::Variant {
                    enum_path: enum_path.clone(),
                    type_args: type_args.clone(),
                    disc: *disc,
                    fields: VariantFields::Tuple(args),
                };
            }
            None => {
                // Unresolved call — typeck would have errored. Use a
                // placeholder path.
                CallTarget::Path(c.callee.segments.iter().map(|s| s.name.clone()).collect())
            }
        };
        Rvalue::Call {
            callee,
            args,
            call_node_id: node_id,
        }
    }

    fn lower_method_call(&mut self, mc: &MethodCall, node_id: ast::NodeId) -> Rvalue {
        // Receiver lowering depends on the method's receiver-adjust:
        // BorrowImm/BorrowMut take an implicit borrow (== `(&recv)` /
        // `(&mut recv)`); Move consumes the owned value; ByRef passes
        // an existing reference through.
        let recv_adjust = self.ctx.method_resolutions[node_id as usize]
            .as_ref()
            .map(|r| r.recv_adjust)
            .unwrap_or(ReceiverAdjust::Move);
        let recv = match recv_adjust {
            ReceiverAdjust::BorrowImm => self.synth_borrow(&mc.receiver, false),
            ReceiverAdjust::BorrowMut => self.synth_borrow(&mc.receiver, true),
            ReceiverAdjust::Move => self.lower_expr_operand(&mc.receiver),
            // ByRef: the receiver is already a ref (`&T` or `&mut T`)
            // and the method takes a ref-typed self. Lower as a
            // **reborrow**: we copy the receiver's i32 ref value into
            // the call instead of consuming the binding. Semantically
            // valid because the callee only borrows for the call's
            // duration; after the call the source binding resumes.
            // Without this, `&mut self` methods couldn't transitively
            // call other `&mut self` methods on the same binding.
            ReceiverAdjust::ByRef => self.lower_recv_reborrow(&mc.receiver),
        };
        let mut args: Vec<Operand> = Vec::new();
        args.push(recv);
        let mut i = 0;
        while i < mc.args.len() {
            args.push(self.lower_expr_operand(&mc.args[i]));
            i += 1;
        }
        Rvalue::Call {
            callee: CallTarget::MethodResolution(node_id),
            args,
            call_node_id: node_id,
        }
    }

    // Reborrow lowering for a method-call receiver that is already a
    // ref (`&T` or `&mut T`). Always emits `Operand::Copy(place)` —
    // even when the place's resolved type is `&mut T` (which is not
    // Copy), copying its i32 representation is sound because the
    // callee scope-bounds the borrow. The original ref-typed binding
    // remains live for use after the call.
    fn lower_recv_reborrow(&mut self, expr: &Expr) -> Operand {
        let span = expr.span.copy();
        let nid = Some(expr.id);
        let place = self.lower_expr_place(expr);
        Operand {
            kind: OperandKind::Copy(place),
            span,
            node_id: nid,
        }
    }

    // Take an implicit borrow of `expr` (used by autoref method
    // dispatch). Materializes through a temp so the call's argument
    // list is uniform Operands.
    fn synth_borrow(&mut self, expr: &Expr, mutable: bool) -> Operand {
        let span = expr.span.copy();
        let place = self.lower_expr_place(expr);
        let region = self.fresh_region();
        let inner_ty = self.expr_type(expr.id);
        let ref_ty = RType::Ref {
            inner: Box::new(inner_ty),
            mutable,
            lifetime: LifetimeRepr::Inferred(0),
        };
        let temp = self.alloc_temp(ref_ty, span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(temp), span.copy());
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(temp),
                rvalue: Rvalue::Borrow {
                    mutable,
                    place,
                    region,
                },
            },
            span.copy(),
        );
        Operand {
            kind: OperandKind::Move(local_place(temp)),
            span,
            node_id: None,
        }
    }

    fn lower_struct_lit(&mut self, lit: &StructLit, node_id: ast::NodeId) -> Rvalue {
        let mut fields: Vec<(String, Operand)> = Vec::new();
        let mut i = 0;
        while i < lit.fields.len() {
            let op = self.lower_expr_operand(&lit.fields[i].value);
            fields.push((lit.fields[i].name.clone(), op));
            i += 1;
        }
        // Struct literal might also be an enum variant struct-form.
        let resolution = self.ctx.call_resolutions[node_id as usize].as_ref();
        if let Some(CallResolution::Variant {
            enum_path,
            disc,
            type_args,
        }) = resolution
        {
            return Rvalue::Variant {
                enum_path: enum_path.clone(),
                type_args: type_args.clone(),
                disc: *disc,
                fields: VariantFields::Struct(fields),
            };
        }
        // Pull out type info from expr_types for type_args.
        let ty = self.expr_type(node_id);
        let (type_path, type_args) = match &ty {
            RType::Struct {
                path, type_args, ..
            } => (path.clone(), type_args.clone()),
            _ => unreachable!("struct lit expr type must be a Struct"),
        };
        Rvalue::StructLit {
            type_path,
            type_args,
            fields,
        }
    }

    // Lower an `if` expression. `target` is Some when the if is on the
    // RHS of an assign / let / has a containing destination.
    fn lower_if_into(
        &mut self,
        if_expr: &ast::IfExpr,
        span: Span,
        target: Option<Place>,
    ) {
        let cond = self.lower_expr_operand(&if_expr.cond);
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge_block = self.new_block();
        self.set_terminator(Terminator::If {
            cond,
            then_block,
            else_block,
        });

        self.current_block = then_block;
        self.lower_block(if_expr.then_block.as_ref(), target.clone());
        self.set_terminator(Terminator::Goto(merge_block));

        self.current_block = else_block;
        self.lower_block(if_expr.else_block.as_ref(), target);
        self.set_terminator(Terminator::Goto(merge_block));

        self.current_block = merge_block;
        let _ = span;
    }

    fn expr_type(&self, id: ast::NodeId) -> RType {
        self.ctx.expr_types[id as usize]
            .as_ref()
            .expect("typeck recorded this expr's type")
            .clone()
    }

    // ---------- Match / if-let lowering ----------

    // Lower a match expression. Each arm: pattern test → arm body block
    // → goto merge. Bindings inside arms are introduced as locals
    // initialized from sub-places of the scrutinee.
    // Lower `'label: while cond { body }`. CFG shape:
    //
    //   prev → cond_block
    //   cond_block: lower(cond); if cond then body_block else after_block
    //   body_block: lower(body); goto cond_block (back-edge)
    //   after_block: continuation
    //
    // `break` in body becomes goto(after_block); `continue` becomes
    // goto(cond_block). Loop frames stack so labelled break/continue
    // can target an outer loop.
    fn lower_while(&mut self, w: &crate::ast::WhileExpr, span: Span) {
        let cond_block = self.new_block();
        let body_block = self.new_block();
        let after_block = self.new_block();
        // Jump from current block to cond_block.
        self.set_terminator(Terminator::Goto(cond_block));

        // Build cond_block: evaluate cond, branch.
        self.current_block = cond_block;
        let cond_op = self.lower_expr_operand(&w.cond);
        self.set_terminator(Terminator::If {
            cond: cond_op,
            then_block: body_block,
            else_block: after_block,
        });

        // Build body_block. Push loop frame for break/continue.
        self.loops.push(LoopFrame {
            label: w.label.clone(),
            continue_target: cond_block,
            break_target: after_block,
        });
        self.current_block = body_block;
        self.lower_block(w.body.as_ref(), None);
        // After body, back-edge to cond_block.
        self.set_terminator(Terminator::Goto(cond_block));
        self.loops.pop();

        // Continuation.
        self.current_block = after_block;
        let _ = span;
    }

    // `for pat in iter { body }` lowers to a CFG that mirrors
    //
    //   let mut __iter = iter;
    //   loop {
    //       // (the actual `Iterator::next(&mut __iter)` call and its
    //       // Some/None destructure happen at codegen time — borrowck
    //       // doesn't model the iterator semantics; it just sees that
    //       // __iter is alive for the whole loop and that the pattern
    //       // bindings are introduced fresh each iteration.)
    //       <pattern bindings>
    //       <body>
    //       continue
    //   }
    //
    // The CFG cond_block uses a synthetic `ConstBool(true)` to keep
    // both successors (body and after) reachable in dataflow —
    // after_block then has incoming edges via that synthetic edge
    // and via any `break` in the body.
    //
    // Pattern bindings are allocated as fresh locals with `StorageLive`
    // but no `Assign`; pocket-rust's move analysis treats places
    // not present in the moved set as init-by-default, so reads of
    // the bindings in the body work without a synthetic source value.
    fn lower_for(&mut self, f: &crate::ast::ForLoop, span: Span) {
        // Move iter into __iter.
        let iter_ty = self.expr_type(f.iter.id);
        let iter_local = self.alloc_temp(iter_ty.clone(), span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(iter_local), span.copy());
        self.lower_expr_into(&f.iter, local_place(iter_local));

        let cond_block = self.new_block();
        let body_block = self.new_block();
        let after_block = self.new_block();
        self.set_terminator(Terminator::Goto(cond_block));

        // cond_block: synthetic branch. The actual exit-on-None lives
        // in codegen; borrowck just needs both successors reachable.
        self.current_block = cond_block;
        self.set_terminator(Terminator::If {
            cond: Operand {
                kind: OperandKind::ConstBool(true),
                span: span.copy(),
                node_id: None,
            },
            then_block: body_block,
            else_block: after_block,
        });

        // body_block: bind pattern (StorageLive only — init by
        // default), lower body, back-edge.
        self.loops.push(LoopFrame {
            label: f.label.clone(),
            continue_target: cond_block,
            break_target: after_block,
        });
        self.current_block = body_block;
        self.push_scope();
        self.materialize_for_loop_bindings(&f.pattern, &iter_ty);
        self.lower_block(f.body.as_ref(), None);
        self.set_terminator(Terminator::Goto(cond_block));
        self.pop_scope();
        self.loops.pop();

        self.current_block = after_block;
        let _ = span;
    }

    // Walk the for-loop's pattern, allocating a local for each
    // binding (with the binding's type derived from the iter's
    // `Iterator::Item` assoc), pushing `StorageLive` for each. No
    // `Assign` is emitted — the move analysis treats unmoved places
    // as initialized, so reads of the bindings in the body work.
    // For-loop pattern shapes are typeck-validated; complex
    // destructure patterns (struct variants etc.) are left as TODO
    // here since they need the same `Item`-source-place machinery
    // that the `if let`/`match` paths use.
    fn materialize_for_loop_bindings(&mut self, pat: &Pattern, iter_ty: &RType) {
        // Resolve `<iter_ty as Iterator>::Item` — same lookup typeck did.
        let iterator_path = vec![
            "std".to_string(),
            "iter".to_string(),
            "Iterator".to_string(),
        ];
        let item_candidates = crate::typeck::find_assoc_binding(
            self.ctx.traits,
            iter_ty,
            &iterator_path,
            "Item",
        );
        let item_ty = item_candidates
            .into_iter()
            .next()
            .expect("typeck verified Iterator impl + Item binding");
        match &pat.kind {
            crate::ast::PatternKind::Wildcard => {}
            crate::ast::PatternKind::Binding { name, name_span, by_ref, mutable } => {
                let local_ty = if *by_ref {
                    RType::Ref {
                        inner: Box::new(item_ty),
                        mutable: *mutable,
                        lifetime: LifetimeRepr::Inferred(0),
                    }
                } else {
                    item_ty
                };
                let local = self.alloc_local(
                    Some(name.clone()),
                    local_ty,
                    name_span.copy(),
                    *mutable && !*by_ref,
                    false,
                );
                self.push_stmt(CfgStmtKind::StorageLive(local), name_span.copy());
                self.bind_name(name, local);
            }
            // TODO: variant / struct / tuple / ref / at-binding /
            // or-patterns inside `for pat in iter` — needs a synthetic
            // `Item`-typed source place that borrowck's existing
            // `materialize_bindings` can destructure against. For now
            // only the bare-binding form (`for x in iter`) and
            // wildcard (`for _ in iter`) are supported by borrowck;
            // typeck accepts any pattern, so destructuring patterns
            // in for-loop position will land here as a borrowck-time
            // panic until this is filled in.
            _ => {
                let _ = item_ty;
                unimplemented!("for-loop pattern destructure not yet supported in borrowck — use `for x in iter` then destructure inside the body");
            }
        }
    }

    fn lower_break(&mut self, label: &Option<String>, span: Span) {
        let target = self.find_loop_break(label.as_deref()).expect("typeck verified break has a target");
        self.set_terminator(Terminator::Goto(target));
        // Code after a break is unreachable. Sink it into a fresh
        // block; the dataflow will ignore it (no incoming edges).
        let unreachable = self.new_block();
        self.current_block = unreachable;
        let _ = span;
    }

    fn lower_continue(&mut self, label: &Option<String>, span: Span) {
        let target = self
            .find_loop_continue(label.as_deref())
            .expect("typeck verified continue has a target");
        self.set_terminator(Terminator::Goto(target));
        let unreachable = self.new_block();
        self.current_block = unreachable;
        let _ = span;
    }

    // `return EXPR` / `return`: lower the value (if any) into the
    // function's return slot (`Local(0)` by convention) and terminate
    // with `Return`. Subsequent code in the same source block sinks
    // into a fresh unreachable block (no incoming edges).
    fn lower_return(&mut self, value: Option<&Expr>, span: Span) {
        if let Some(e) = value {
            // Pretend we're assigning to local 0 — pocket-rust's
            // existing CFG doesn't have a separate return-slot
            // concept, but the eventual `Terminator::Return` will be
            // honored regardless. We just need the move/borrow
            // checker to see that `e` was read.
            let _ = self.lower_expr_operand(e);
        }
        self.set_terminator(Terminator::Return);
        let unreachable = self.new_block();
        self.current_block = unreachable;
        let _ = span;
    }

    fn find_loop_break(&self, label: Option<&str>) -> Option<BlockId> {
        match label {
            None => self.loops.last().map(|f| f.break_target),
            Some(name) => {
                let mut i = self.loops.len();
                while i > 0 {
                    i -= 1;
                    if self.loops[i].label.as_deref() == Some(name) {
                        return Some(self.loops[i].break_target);
                    }
                }
                None
            }
        }
    }

    fn find_loop_continue(&self, label: Option<&str>) -> Option<BlockId> {
        match label {
            None => self.loops.last().map(|f| f.continue_target),
            Some(name) => {
                let mut i = self.loops.len();
                while i > 0 {
                    i -= 1;
                    if self.loops[i].label.as_deref() == Some(name) {
                        return Some(self.loops[i].continue_target);
                    }
                }
                None
            }
        }
    }

    fn lower_match_into(&mut self, m: &MatchExpr, target: Option<Place>) {
        // Evaluate scrutinee into a temp so the same place can be tested
        // and bound from across all arms.
        let scrut_ty = self.expr_type(m.scrutinee.id);
        let scrut_local = self.alloc_temp(scrut_ty.clone(), m.scrutinee.span.copy());
        self.push_stmt(
            CfgStmtKind::StorageLive(scrut_local),
            m.scrutinee.span.copy(),
        );
        self.lower_expr_into(&m.scrutinee, local_place(scrut_local));

        let merge_block = self.new_block();
        let scrut_place = local_place(scrut_local);

        let mut i = 0;
        while i < m.arms.len() {
            let arm = &m.arms[i];
            let arm_body_block = self.new_block();
            // After this arm fails, control flows to either the next
            // arm's test (if any) or to an Unreachable block (typeck
            // enforces exhaustiveness).
            let next_block = if i + 1 < m.arms.len() {
                self.new_block()
            } else {
                let u = self.new_block();
                let saved = self.current_block;
                self.current_block = u;
                self.set_terminator(Terminator::Unreachable);
                self.current_block = saved;
                u
            };
            self.lower_pattern_test(
                &arm.pattern,
                &scrut_place,
                &scrut_ty,
                arm_body_block,
                next_block,
            );
            // Build the arm body block.
            self.current_block = arm_body_block;
            self.push_scope();
            let bindings = self.collect_pattern_bindings(&arm.pattern, &scrut_place, &scrut_ty);
            self.materialize_bindings(&bindings);
            // Guard, if present.
            if let Some(g) = &arm.guard {
                let guard_op = self.lower_expr_operand(g);
                let guard_pass_block = self.new_block();
                self.set_terminator(Terminator::If {
                    cond: guard_op,
                    then_block: guard_pass_block,
                    else_block: next_block,
                });
                self.current_block = guard_pass_block;
            }
            // Lower the arm body into target.
            if let Some(t) = target.clone() {
                self.lower_expr_into(&arm.body, t);
            } else {
                let _ = self.lower_expr_operand(&arm.body);
            }
            self.set_terminator(Terminator::Goto(merge_block));
            self.pop_scope();

            self.current_block = next_block;
            i += 1;
        }

        // After all arms, the next_block of the last arm is Unreachable.
        // Continue building from merge_block.
        self.current_block = merge_block;
    }

    fn lower_if_let_into(&mut self, il: &IfLetExpr, target: Option<Place>) {
        let scrut_ty = self.expr_type(il.scrutinee.id);
        let scrut_local = self.alloc_temp(scrut_ty.clone(), il.scrutinee.span.copy());
        self.push_stmt(
            CfgStmtKind::StorageLive(scrut_local),
            il.scrutinee.span.copy(),
        );
        self.lower_expr_into(&il.scrutinee, local_place(scrut_local));

        let merge_block = self.new_block();
        let then_body_block = self.new_block();
        let else_block = self.new_block();
        let scrut_place = local_place(scrut_local);

        self.lower_pattern_test(
            &il.pattern,
            &scrut_place,
            &scrut_ty,
            then_body_block,
            else_block,
        );

        // then-branch: bind pattern, run then-block.
        self.current_block = then_body_block;
        self.push_scope();
        let bindings = self.collect_pattern_bindings(&il.pattern, &scrut_place, &scrut_ty);
        self.materialize_bindings(&bindings);
        self.lower_block(il.then_block.as_ref(), target.clone());
        self.set_terminator(Terminator::Goto(merge_block));
        self.pop_scope();

        // else-branch: just run the else-block.
        self.current_block = else_block;
        self.lower_block(il.else_block.as_ref(), target);
        self.set_terminator(Terminator::Goto(merge_block));

        self.current_block = merge_block;
    }

    // Emit CFG operations that test `pat` against `scrut_place` (whose
    // value has type `scrut_ty`). Sets the current block's terminator
    // to branch to `on_match` on success or `on_fail` on failure. After
    // returning, the current block is the test's last test block (the
    // one whose terminator was just set).
    fn lower_pattern_test(
        &mut self,
        pat: &Pattern,
        scrut_place: &Place,
        scrut_ty: &RType,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        match &pat.kind {
            PatternKind::Wildcard
            | PatternKind::Binding { .. } => {
                self.set_terminator(Terminator::Goto(on_match));
            }
            PatternKind::At { inner, .. } => {
                self.lower_pattern_test(inner, scrut_place, scrut_ty, on_match, on_fail);
            }
            PatternKind::LitInt(n) => {
                let op = self.move_or_copy(scrut_place, scrut_ty.clone(), pat.span.copy(), None);
                self.set_terminator(Terminator::SwitchInt {
                    operand: op,
                    targets: vec![(*n, on_match)],
                    otherwise: on_fail,
                });
            }
            PatternKind::LitBool(b) => {
                let op = self.move_or_copy(scrut_place, scrut_ty.clone(), pat.span.copy(), None);
                let (then_b, else_b) = if *b {
                    (on_match, on_fail)
                } else {
                    (on_fail, on_match)
                };
                self.set_terminator(Terminator::If {
                    cond: op,
                    then_block: then_b,
                    else_block: else_b,
                });
            }
            PatternKind::Range { lo, hi } => {
                // Emit chained tests: lo <= scrut && scrut <= hi.
                // For simplicity, use a SwitchInt with every value in
                // the range as a target. Acceptable for small ranges
                // (which is the typical usage); larger ranges should
                // emit comparison ops, but pocket-rust doesn't have
                // those as Rvalues yet.
                let op = self.move_or_copy(scrut_place, scrut_ty.clone(), pat.span.copy(), None);
                let mut targets: Vec<(u64, BlockId)> = Vec::new();
                let mut v = *lo;
                while v <= *hi {
                    targets.push((v, on_match));
                    if v == u64::MAX {
                        break;
                    }
                    v += 1;
                }
                self.set_terminator(Terminator::SwitchInt {
                    operand: op,
                    targets,
                    otherwise: on_fail,
                });
            }
            PatternKind::Ref { inner, .. } => {
                let pointee_ty = match scrut_ty {
                    RType::Ref { inner, .. } => (**inner).clone(),
                    _ => unreachable!("typeck verified Ref-pattern scrutinee"),
                };
                let mut sub = scrut_place.clone();
                sub.projections.push(Projection::Deref);
                self.lower_pattern_test(inner, &sub, &pointee_ty, on_match, on_fail);
            }
            PatternKind::Tuple(elems) => {
                let elem_types: Vec<RType> = match scrut_ty {
                    RType::Tuple(es) => es.clone(),
                    _ => unreachable!("typeck verified Tuple-pattern scrutinee"),
                };
                self.test_sequence(elems, scrut_place, &elem_types, |i| {
                    Projection::TupleIndex(i as u32)
                }, on_match, on_fail);
            }
            PatternKind::VariantTuple { path, elems } => {
                self.test_variant_payload(
                    path,
                    scrut_place,
                    scrut_ty,
                    pat.span.copy(),
                    on_match,
                    on_fail,
                    |b, payload, sub_place, on_match, on_fail| {
                        let payload_types: Vec<RType> = match payload {
                            VariantPayloadResolved::Tuple(types) => types,
                            VariantPayloadResolved::Unit if elems.is_empty() => Vec::new(),
                            _ => unreachable!("typeck mismatch"),
                        };
                        b.test_sequence(elems, sub_place, &payload_types, |i| {
                            Projection::TupleIndex(i as u32)
                        }, on_match, on_fail);
                    },
                );
            }
            PatternKind::VariantStruct { path, fields, rest: _ } => {
                match scrut_ty {
                    RType::Enum {
                        path: enum_path,
                        type_args,
                        ..
                    } => {
                        // Variant struct: disc test, then field tests.
                        let variant_name = &path.segments[path.segments.len() - 1].name;
                        let variant = lookup_variant_in_enum(self.ctx.enums, enum_path, variant_name)
                            .expect("typeck verified variant exists")
                            .clone();
                        let env = enum_type_env(self.ctx.enums, enum_path, type_args);
                        let payload_fields = match &variant.payload {
                            VariantPayloadResolved::Struct(fs) => fs.clone(),
                            _ => unreachable!("typeck verified variant struct"),
                        };
                        // Disc test.
                        let disc_local =
                            self.alloc_temp(RType::Int(IntKind::I32), pat.span.copy());
                        self.push_stmt(CfgStmtKind::StorageLive(disc_local), pat.span.copy());
                        self.push_stmt(
                            CfgStmtKind::Assign {
                                place: local_place(disc_local),
                                rvalue: Rvalue::Discriminant(scrut_place.clone()),
                            },
                            pat.span.copy(),
                        );
                        let payload_match_block = self.new_block();
                        self.set_terminator(Terminator::SwitchInt {
                            operand: Operand {
                                kind: OperandKind::Move(local_place(disc_local)),
                                span: pat.span.copy(),
                                node_id: None,
                            },
                            targets: vec![(variant.disc as u64, payload_match_block)],
                            otherwise: on_fail,
                        });
                        self.current_block = payload_match_block;
                        self.test_struct_fields(
                            fields,
                            scrut_place,
                            &payload_fields,
                            &env,
                            on_match,
                            on_fail,
                        );
                    }
                    RType::Struct {
                        path: spath,
                        type_args,
                        ..
                    } => {
                        // Plain struct destructure: no disc test, just
                        // field tests.
                        let entry = crate::typeck::struct_lookup(self.ctx.structs, spath)
                            .expect("typeck verified struct");
                        let payload_fields: Vec<crate::typeck::RTypedField> = entry
                            .fields
                            .iter()
                            .map(|f| crate::typeck::RTypedField {
                                name: f.name.clone(),
                                name_span: f.name_span.copy(),
                                ty: f.ty.clone(),
                                is_pub: f.is_pub,
                            })
                            .collect();
                        let mut env: Vec<(String, RType)> = Vec::new();
                        let mut k = 0;
                        while k < entry.type_params.len() && k < type_args.len() {
                            env.push((entry.type_params[k].clone(), type_args[k].clone()));
                            k += 1;
                        }
                        self.test_struct_fields(
                            fields,
                            scrut_place,
                            &payload_fields,
                            &env,
                            on_match,
                            on_fail,
                        );
                    }
                    _ => unreachable!("typeck verified VariantStruct scrutinee"),
                }
            }
            PatternKind::Or(alts) => {
                let mut idx = 0;
                while idx < alts.len() {
                    let next = if idx + 1 < alts.len() {
                        self.new_block()
                    } else {
                        on_fail
                    };
                    self.lower_pattern_test(
                        &alts[idx],
                        scrut_place,
                        scrut_ty,
                        on_match,
                        next,
                    );
                    self.current_block = next;
                    idx += 1;
                }
            }
        }
    }

    // Sequential tests over named struct/variant-struct fields. Each
    // field's type comes from the declared payload after substituting
    // the enum/struct type-arg env.
    fn test_struct_fields(
        &mut self,
        fields: &Vec<ast::FieldPattern>,
        scrut_place: &Place,
        payload_fields: &Vec<crate::typeck::RTypedField>,
        env: &Vec<(String, RType)>,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        if fields.is_empty() {
            self.set_terminator(Terminator::Goto(on_match));
            return;
        }
        let mut idx = 0;
        while idx < fields.len() {
            let next = if idx + 1 < fields.len() {
                self.new_block()
            } else {
                on_match
            };
            let fname = &fields[idx].name;
            let fty = payload_fields
                .iter()
                .find(|f| f.name == *fname)
                .map(|f| substitute_rtype(&f.ty, env))
                .expect("typeck verified field");
            let mut sub = scrut_place.clone();
            sub.projections.push(Projection::Field(fname.clone()));
            self.lower_pattern_test(&fields[idx].pattern, &sub, &fty, next, on_fail);
            self.current_block = next;
            idx += 1;
        }
    }

    // Helper for sequential pattern tests over a positional list of
    // sub-patterns (tuples and tuple variants). Each test branches to
    // the next test on success or to `on_fail` on failure; the last
    // success branch goes to `on_match`.
    fn test_sequence(
        &mut self,
        pats: &Vec<Pattern>,
        scrut_place: &Place,
        elem_tys: &Vec<RType>,
        proj: impl Fn(usize) -> Projection,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        if pats.is_empty() {
            self.set_terminator(Terminator::Goto(on_match));
            return;
        }
        let mut idx = 0;
        while idx < pats.len() {
            let next = if idx + 1 < pats.len() {
                self.new_block()
            } else {
                on_match
            };
            let mut sub = scrut_place.clone();
            sub.projections.push(proj(idx));
            self.lower_pattern_test(&pats[idx], &sub, &elem_tys[idx], next, on_fail);
            self.current_block = next;
            idx += 1;
        }
    }

    // Helper for variant patterns: emit the discriminant test, then
    // hand the payload sub-place + variant payload metadata to a
    // callback that recurses on the payload pattern. The callback runs
    // in the `payload_match_block`, which is reached only if the
    // discriminant matches.
    fn test_variant_payload<F>(
        &mut self,
        path: &ast::Path,
        scrut_place: &Place,
        scrut_ty: &RType,
        span: Span,
        on_match: BlockId,
        on_fail: BlockId,
        recurse: F,
    ) where
        F: FnOnce(&mut Self, VariantPayloadResolved, &Place, BlockId, BlockId),
    {
        // Resolve variant from path against the scrut's enum type.
        let (enum_path_canonical, type_args) = match scrut_ty {
            RType::Enum {
                path,
                type_args,
                ..
            } => (path.clone(), type_args.clone()),
            // Handle plain struct (not enum) — fall through to the
            // recurse step with no disc test.
            RType::Struct { .. } => {
                recurse(
                    self,
                    VariantPayloadResolved::Unit,
                    scrut_place,
                    on_match,
                    on_fail,
                );
                return;
            }
            _ => unreachable!("typeck verified variant pattern scrutinee"),
        };

        // Find the matching variant by name within the scrutinee's
        // canonical enum.
        let variant_name = &path.segments[path.segments.len() - 1].name;
        let variant = lookup_variant_in_enum(self.ctx.enums, &enum_path_canonical, variant_name)
            .expect("typeck verified variant exists");
        let env = enum_type_env(self.ctx.enums, &enum_path_canonical, &type_args);
        let disc = variant.disc;
        let variant = variant.clone();
        let payload = substitute_variant_payload(&variant.payload, &env);

        // Read discriminant.
        let disc_local = self.alloc_temp(RType::Int(IntKind::I32), span.copy());
        self.push_stmt(CfgStmtKind::StorageLive(disc_local), span.copy());
        self.push_stmt(
            CfgStmtKind::Assign {
                place: local_place(disc_local),
                rvalue: Rvalue::Discriminant(scrut_place.clone()),
            },
            span.copy(),
        );
        let payload_match_block = self.new_block();
        self.set_terminator(Terminator::SwitchInt {
            operand: Operand {
                kind: OperandKind::Move(local_place(disc_local)),
                span: span.copy(),
                node_id: None,
            },
            targets: vec![(disc as u64, payload_match_block)],
            otherwise: on_fail,
        });
        self.current_block = payload_match_block;
        recurse(self, payload, scrut_place, on_match, on_fail);
    }

    // ---------- Pattern bindings ----------

    // Walk a pattern, collecting bindings: each (name, sub-place,
    // by_ref, mutable, type). The sub-place is the path from
    // `scrut_place` to the pattern position.
    fn collect_pattern_bindings(
        &mut self,
        pat: &Pattern,
        scrut_place: &Place,
        scrut_ty: &RType,
    ) -> Vec<PatternBinding> {
        let mut out: Vec<PatternBinding> = Vec::new();
        self.collect_bindings_into(pat, scrut_place, scrut_ty, &mut out);
        out
    }

    fn collect_bindings_into(
        &mut self,
        pat: &Pattern,
        scrut_place: &Place,
        scrut_ty: &RType,
        out: &mut Vec<PatternBinding>,
    ) {
        match &pat.kind {
            PatternKind::Wildcard
            | PatternKind::LitInt(_)
            | PatternKind::LitBool(_)
            | PatternKind::Range { .. } => {}
            PatternKind::Binding {
                name,
                name_span,
                by_ref,
                mutable,
            } => {
                out.push(PatternBinding {
                    name: name.clone(),
                    place: scrut_place.clone(),
                    ty: scrut_ty.clone(),
                    by_ref: *by_ref,
                    mutable: *mutable,
                    span: name_span.copy(),
                });
            }
            PatternKind::At {
                name,
                name_span,
                inner,
            } => {
                out.push(PatternBinding {
                    name: name.clone(),
                    place: scrut_place.clone(),
                    ty: scrut_ty.clone(),
                    by_ref: false,
                    mutable: false,
                    span: name_span.copy(),
                });
                self.collect_bindings_into(inner, scrut_place, scrut_ty, out);
            }
            PatternKind::Ref { inner, .. } => {
                let pointee_ty = match scrut_ty {
                    RType::Ref { inner, .. } => (**inner).clone(),
                    _ => return,
                };
                let mut sub = scrut_place.clone();
                sub.projections.push(Projection::Deref);
                self.collect_bindings_into(inner, &sub, &pointee_ty, out);
            }
            PatternKind::Tuple(elems) => {
                let elem_tys: Vec<RType> = match scrut_ty {
                    RType::Tuple(es) => es.clone(),
                    _ => return,
                };
                let mut i = 0;
                while i < elems.len() && i < elem_tys.len() {
                    let mut sub = scrut_place.clone();
                    sub.projections.push(Projection::TupleIndex(i as u32));
                    self.collect_bindings_into(&elems[i], &sub, &elem_tys[i], out);
                    i += 1;
                }
            }
            PatternKind::VariantTuple { path, elems } => {
                if let RType::Enum {
                    path: enum_path,
                    type_args,
                    ..
                } = scrut_ty
                {
                    let variant_name = &path.segments[path.segments.len() - 1].name;
                    let env = enum_type_env(self.ctx.enums, enum_path, type_args);
                    if let Some(variant) =
                        lookup_variant_in_enum(self.ctx.enums, enum_path, variant_name)
                    {
                        if let VariantPayloadResolved::Tuple(types) = &variant.payload {
                            let mut i = 0;
                            while i < elems.len() && i < types.len() {
                                let ty = substitute_rtype(&types[i], &env);
                                let mut sub = scrut_place.clone();
                                sub.projections.push(Projection::TupleIndex(i as u32));
                                self.collect_bindings_into(&elems[i], &sub, &ty, out);
                                i += 1;
                            }
                        }
                    }
                }
            }
            PatternKind::VariantStruct {
                path,
                fields,
                rest: _,
            } => {
                let (payload_fields, env) = match scrut_ty {
                    RType::Enum {
                        path: enum_path,
                        type_args,
                        ..
                    } => {
                        let variant_name = &path.segments[path.segments.len() - 1].name;
                        let env = enum_type_env(self.ctx.enums, enum_path, type_args);
                        let pf = lookup_variant_in_enum(self.ctx.enums, enum_path, variant_name)
                            .and_then(|v| match &v.payload {
                                VariantPayloadResolved::Struct(fs) => Some(fs.clone()),
                                _ => None,
                            });
                        (pf, env)
                    }
                    RType::Struct {
                        path: spath,
                        type_args,
                        ..
                    } => {
                        let pf = crate::typeck::struct_lookup(self.ctx.structs, spath).map(|s| {
                            let mut env: Vec<(String, RType)> = Vec::new();
                            let mut k = 0;
                            while k < s.type_params.len() && k < type_args.len() {
                                env.push((s.type_params[k].clone(), type_args[k].clone()));
                                k += 1;
                            }
                            (
                                s.fields
                                    .iter()
                                    .map(|f| crate::typeck::RTypedField {
                                        name: f.name.clone(),
                                        name_span: f.name_span.copy(),
                                        ty: f.ty.clone(),
                                        is_pub: f.is_pub,
                                    })
                                    .collect::<Vec<_>>(),
                                env,
                            )
                        });
                        match pf {
                            Some((fs, env)) => (Some(fs), env),
                            None => (None, Vec::<(String, RType)>::new()),
                        }
                    }
                    _ => (None, Vec::<(String, RType)>::new()),
                };
                if let Some(payload_fields) = payload_fields {
                    let mut i = 0;
                    while i < fields.len() {
                        let fname = &fields[i].name;
                        let fty = payload_fields
                            .iter()
                            .find(|f| f.name == *fname)
                            .map(|f| substitute_rtype(&f.ty, &env));
                        if let Some(fty) = fty {
                            let mut sub = scrut_place.clone();
                            sub.projections.push(Projection::Field(fname.clone()));
                            self.collect_bindings_into(&fields[i].pattern, &sub, &fty, out);
                        }
                        i += 1;
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Or-patterns must bind the same names on every alt;
                // typeck enforced this. Walk only the first alt — its
                // binding shape is canonical.
                if !alts.is_empty() {
                    self.collect_bindings_into(&alts[0], scrut_place, scrut_ty, out);
                }
            }
        }
    }

    // Materialize the bindings: alloc a local for each, emit an
    // initializing Assign (move/copy or borrow), and bind the name in
    // the current scope.
    fn materialize_bindings(&mut self, bindings: &[PatternBinding]) {
        let mut i = 0;
        while i < bindings.len() {
            let b = &bindings[i];
            let local_ty = if b.by_ref {
                RType::Ref {
                    inner: Box::new(b.ty.clone()),
                    mutable: b.mutable,
                    lifetime: LifetimeRepr::Inferred(0),
                }
            } else {
                b.ty.clone()
            };
            let local = self.alloc_local(
                Some(b.name.clone()),
                local_ty,
                b.span.copy(),
                b.mutable && !b.by_ref,
                false,
            );
            self.push_stmt(CfgStmtKind::StorageLive(local), b.span.copy());
            let rvalue = if b.by_ref {
                let region = self.fresh_region();
                Rvalue::Borrow {
                    mutable: b.mutable,
                    place: b.place.clone(),
                    region,
                }
            } else {
                Rvalue::Use(self.move_or_copy(&b.place, b.ty.clone(), b.span.copy(), None))
            };
            self.push_stmt(
                CfgStmtKind::Assign {
                    place: local_place(local),
                    rvalue,
                },
                b.span.copy(),
            );
            self.bind_name(&b.name, local);
            i += 1;
        }
    }
}

struct PatternBinding {
    name: String,
    place: Place,
    ty: RType,
    by_ref: bool,
    mutable: bool,
    span: Span,
}

// Find a variant within a known enum (by canonical enum path) by name.
// Returns the variant entry. Used after the scrutinee's enum is known
// from its RType; avoids reaching into use-scope/reexport machinery.
fn lookup_variant_in_enum<'a>(
    enums: &'a EnumTable,
    enum_path: &Vec<String>,
    variant_name: &str,
) -> Option<&'a EnumVariantEntry> {
    let mut i = 0;
    while i < enums.entries.len() {
        if enums.entries[i].path == *enum_path {
            let mut j = 0;
            while j < enums.entries[i].variants.len() {
                if enums.entries[i].variants[j].name == variant_name {
                    return Some(&enums.entries[i].variants[j]);
                }
                j += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

// Build a substitution env from the scrutinee's enum type-args + the
// enum's type-param names.
fn enum_type_env(enums: &EnumTable, enum_path: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let mut i = 0;
    while i < enums.entries.len() {
        if enums.entries[i].path == *enum_path {
            let mut k = 0;
            while k < enums.entries[i].type_params.len() && k < type_args.len() {
                env.push((
                    enums.entries[i].type_params[k].clone(),
                    type_args[k].clone(),
                ));
                k += 1;
            }
            return env;
        }
        i += 1;
    }
    env
}

fn substitute_variant_payload(
    payload: &VariantPayloadResolved,
    env: &Vec<(String, RType)>,
) -> VariantPayloadResolved {
    match payload {
        VariantPayloadResolved::Unit => VariantPayloadResolved::Unit,
        VariantPayloadResolved::Tuple(types) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < types.len() {
                out.push(substitute_rtype(&types[i], env));
                i += 1;
            }
            VariantPayloadResolved::Tuple(out)
        }
        VariantPayloadResolved::Struct(fields) => {
            let mut out: Vec<crate::typeck::RTypedField> = Vec::new();
            let mut i = 0;
            while i < fields.len() {
                out.push(crate::typeck::RTypedField {
                    name: fields[i].name.clone(),
                    name_span: fields[i].name_span.copy(),
                    ty: substitute_rtype(&fields[i].ty, env),
                    is_pub: fields[i].is_pub,
                });
                i += 1;
            }
            VariantPayloadResolved::Struct(out)
        }
    }
}

fn local_place(local: LocalId) -> Place {
    Place {
        root: local,
        projections: Vec::new(),
    }
}

fn unit_rtype() -> RType {
    RType::Tuple(Vec::new())
}

fn is_unit(rt: &RType) -> bool {
    matches!(rt, RType::Tuple(elems) if elems.is_empty())
}

