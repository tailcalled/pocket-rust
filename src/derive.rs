// Derive-expansion stage. Sits between the parser and module resolution:
// walks the parser's `Vec<RawItem>`, finds struct/enum decls carrying
// `#[derive(...)]` clauses, and synthesizes the corresponding trait
// impls as additional `RawItem::Impl` entries inserted right after the
// target def.
//
// Synthesis is direct AST construction — no source strings or re-parse
// passes. Each builder constructs the impl's `ImplBlock`, including
// methods with full `Block`/`Expr`/`Pattern` trees. NodeIds are
// allocated per-method on a fresh counter so each generated `Function`
// carries the correct `node_count` (downstream side-vectors size off
// it).
//
// Scope:
//   - Targets: named-field structs (incl. zero-field), unit structs,
//     and enums (any variant payload shape).
//   - Traits: Copy, Clone, PartialEq, Eq, PartialOrd, Ord.
//   - Generic targets: every declared type-param gets a `T: Trait`
//     bound on the derived impl. Lifetime params pass through
//     unbounded.
//   - PartialOrd/Ord on enums is rejected with a diagnostic — would
//     need explicit discriminant comparison machinery; defer.

use crate::ast::{
    Block, DeriveTrait, EnumDef, EnumVariant, Expr, ExprKind, FieldInit, FieldPattern, Function,
    IfExpr, ImplBlock, LifetimeParam, MatchArm, MatchExpr, MethodCall, NodeId, Param, Path,
    PathSegment, Pattern, PatternKind, Stmt, StructDef, StructLit, TraitBound, Type, TypeKind,
    TypeParam, VariantPayload,
};
use crate::parser::RawItem;
use crate::span::{Error, Span};

pub fn expand(file: &str, items: Vec<RawItem>) -> Result<Vec<RawItem>, Error> {
    let mut out: Vec<RawItem> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            RawItem::Struct(sd) => {
                let derives = sd.derives.clone();
                out.push(RawItem::Struct(sd.clone()));
                for clause in &derives {
                    for trait_ref in &clause.traits {
                        let imp = synthesize_struct_impl(file, &sd, trait_ref)?;
                        out.push(RawItem::Impl(imp));
                    }
                }
            }
            RawItem::Enum(ed) => {
                let derives = ed.derives.clone();
                out.push(RawItem::Enum(ed.clone()));
                for clause in &derives {
                    for trait_ref in &clause.traits {
                        let imp = synthesize_enum_impl(file, &ed, trait_ref)?;
                        out.push(RawItem::Impl(imp));
                    }
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

// =====================================================================
// Builder — incremental NodeId allocator + AST constructors. One
// Builder per synthesized method body so each `Function.node_count`
// captures the correct count.
// =====================================================================

struct Builder {
    next_id: NodeId,
    span: Span,
}

impl Builder {
    fn new(span: Span) -> Self {
        Builder { next_id: 0, span }
    }

    fn fresh_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn s(&self) -> Span {
        self.span.copy()
    }

    fn expr(&mut self, kind: ExprKind) -> Expr {
        let id = self.fresh_id();
        Expr { kind, span: self.s(), id }
    }

    fn pat(&mut self, kind: PatternKind) -> Pattern {
        let id = self.fresh_id();
        Pattern { kind, span: self.s(), id }
    }

    fn block(&mut self, stmts: Vec<Stmt>, tail: Option<Expr>) -> Block {
        Block { stmts, tail, span: self.s() }
    }

    fn var(&mut self, name: &str) -> Expr {
        self.expr(ExprKind::Var(name.to_string()))
    }

    fn field_access(&mut self, base: Expr, field: &str) -> Expr {
        self.expr(ExprKind::FieldAccess(crate::ast::FieldAccess {
            base: Box::new(base),
            field: field.to_string(),
            field_span: self.s(),
        }))
    }

    fn method_call(&mut self, recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
        self.expr(ExprKind::MethodCall(MethodCall {
            receiver: Box::new(recv),
            method: method.to_string(),
            method_span: self.s(),
            turbofish_args: Vec::new(),
            args,
        }))
    }

    fn borrow(&mut self, inner: Expr, mutable: bool) -> Expr {
        self.expr(ExprKind::Borrow { inner: Box::new(inner), mutable })
    }

    fn bool_lit(&mut self, b: bool) -> Expr {
        self.expr(ExprKind::BoolLit(b))
    }

    fn struct_lit(&mut self, path: Path, fields: Vec<(String, Expr)>) -> Expr {
        let mut field_inits: Vec<FieldInit> = Vec::new();
        for (name, value) in fields {
            field_inits.push(FieldInit {
                name,
                name_span: self.s(),
                value,
            });
        }
        self.expr(ExprKind::StructLit(StructLit {
            path,
            fields: field_inits,
        }))
    }

    fn variant_call(&mut self, path: Path, args: Vec<Expr>) -> Expr {
        self.expr(ExprKind::Call(crate::ast::Call { callee: path, args }))
    }

    fn if_then_else(&mut self, cond: Expr, then_block: Block, else_block: Block) -> Expr {
        self.expr(ExprKind::If(IfExpr {
            cond: Box::new(cond),
            then_block: Box::new(then_block),
            else_block: Box::new(else_block),
        }))
    }

    fn return_expr(&mut self, value: Option<Expr>) -> Expr {
        self.expr(ExprKind::Return {
            value: value.map(Box::new),
        })
    }

    fn match_expr(&mut self, scrutinee: Expr, arms: Vec<MatchArm>) -> Expr {
        let span = self.s();
        self.expr(ExprKind::Match(MatchExpr {
            scrutinee: Box::new(scrutinee),
            arms,
            span,
        }))
    }

    fn builtin(&mut self, name: &str, args: Vec<Expr>) -> Expr {
        self.expr(ExprKind::Builtin {
            name: name.to_string(),
            name_span: self.s(),
            type_args: Vec::new(),
            args,
        })
    }

    fn binding_pat(&mut self, name: &str) -> Pattern {
        self.pat(PatternKind::Binding {
            name: name.to_string(),
            name_span: self.s(),
            by_ref: false,
            mutable: false,
        })
    }

    fn wildcard_pat(&mut self) -> Pattern {
        self.pat(PatternKind::Wildcard)
    }

    fn variant_unit_pat(&mut self, path: Path) -> Pattern {
        // Unit variants in patterns parse as VariantTuple with empty
        // elems (typeck dispatches to the unit-payload branch).
        self.pat(PatternKind::VariantTuple { path, elems: Vec::new() })
    }

    fn variant_tuple_pat(&mut self, path: Path, elems: Vec<Pattern>) -> Pattern {
        self.pat(PatternKind::VariantTuple { path, elems })
    }

    fn variant_struct_pat(&mut self, path: Path, fields: Vec<FieldPattern>) -> Pattern {
        self.pat(PatternKind::VariantStruct {
            path,
            fields,
            rest: false,
        })
    }
}

// Build a Path with one segment per name and the final segment carrying
// any type-args. All spans use the derive-attribute span.
fn mk_path(segments: &[&str], type_args: Vec<Type>, span: &Span) -> Path {
    let mut segs: Vec<PathSegment> = Vec::new();
    for (i, name) in segments.iter().enumerate() {
        let args = if i + 1 == segments.len() { type_args.clone() } else { Vec::new() };
        segs.push(PathSegment {
            name: name.to_string(),
            span: span.copy(),
            lifetime_args: Vec::new(),
            args,
        });
    }
    Path { segments: segs, span: span.copy() }
}

fn mk_type_path(path: Path) -> Type {
    let span = path.span.copy();
    Type { kind: TypeKind::Path(path), span }
}

fn mk_ref_type(inner: Type, mutable: bool) -> Type {
    let span = inner.span.copy();
    Type {
        kind: TypeKind::Ref {
            inner: Box::new(inner),
            mutable,
            lifetime: None,
        },
        span,
    }
}

fn mk_self_type(span: &Span) -> Type {
    Type { kind: TypeKind::SelfType, span: span.copy() }
}

fn mk_bool_type(span: &Span) -> Type {
    mk_type_path(mk_path(&["bool"], Vec::new(), span))
}

// Bounded type params for the derived impl: clone the target's type
// params and add `Trait` to each one's bounds. Lifetime params pass
// through unbounded.
fn render_impl_generics(
    target_type_params: &Vec<TypeParam>,
    target_lifetime_params: &Vec<LifetimeParam>,
    bound_trait: &str,
    span: &Span,
) -> (Vec<LifetimeParam>, Vec<TypeParam>) {
    let lifetime_params = target_lifetime_params
        .iter()
        .map(|lp| LifetimeParam {
            name: lp.name.clone(),
            name_span: lp.name_span.copy(),
        })
        .collect();
    let mut bound_path = mk_path(&[bound_trait], Vec::new(), span);
    bound_path.segments[0].span = span.copy();
    let type_params = target_type_params
        .iter()
        .map(|tp| {
            let mut bounds = tp.bounds.clone();
            bounds.push(TraitBound {
                path: bound_path.clone(),
                assoc_constraints: Vec::new(),
            });
            TypeParam {
                name: tp.name.clone(),
                name_span: tp.name_span.copy(),
                bounds,
                default: tp.default.clone(),
            }
        })
        .collect();
    (lifetime_params, type_params)
}

// Construct the `target_type` reference for the impl — `Foo<T1, T2>` /
// `Bar<'a, T>`. Built from the target def's name + its type params (as
// `Type::Path` segments). Lifetime params are not currently propagated
// into the type-args slot; pocket-rust's existing impls use bare paths
// without lifetime args in the type position, and the lifetime params
// declared on the impl's `<...>` cover any references inside.
fn target_type_for(name: &str, type_params: &Vec<TypeParam>, span: &Span) -> Type {
    let type_args: Vec<Type> = type_params
        .iter()
        .map(|tp| mk_type_path(mk_path(&[&tp.name], Vec::new(), span)))
        .collect();
    mk_type_path(mk_path(&[name], type_args, span))
}

// Self-referential type for the synthesized method's return and arg
// slots: same as `target_type_for` but used in body positions where
// `Self` would also work. Using the concrete name keeps generated
// errors more specific.
fn self_target_type(name: &str, type_params: &Vec<TypeParam>, span: &Span) -> Type {
    target_type_for(name, type_params, span)
}

fn mk_self_param(span: &Span) -> Param {
    Param {
        name: "self".to_string(),
        name_span: span.copy(),
        ty: mk_ref_type(mk_self_type(span), false),
    }
}

fn mk_other_param(target: &Type, span: &Span) -> Param {
    Param {
        name: "other".to_string(),
        name_span: span.copy(),
        ty: mk_ref_type(target.clone(), false),
    }
}


// =====================================================================
// Per-trait synthesis dispatch
// =====================================================================

fn synthesize_struct_impl(
    file: &str,
    sd: &StructDef,
    trait_ref: &DeriveTrait,
) -> Result<ImplBlock, Error> {
    match trait_ref.name.as_str() {
        "Copy" => Ok(empty_marker_impl(
            &sd.lifetime_params,
            &sd.type_params,
            &sd.name,
            "Copy",
            &trait_ref.name_span,
        )),
        "Eq" => Ok(empty_marker_impl(
            &sd.lifetime_params,
            &sd.type_params,
            &sd.name,
            "Eq",
            &trait_ref.name_span,
        )),
        "Ord" => Ok(empty_marker_impl(
            &sd.lifetime_params,
            &sd.type_params,
            &sd.name,
            "Ord",
            &trait_ref.name_span,
        )),
        "Clone" => Ok(struct_clone_impl(sd, &trait_ref.name_span)),
        "PartialEq" => Ok(struct_partial_eq_impl(sd, &trait_ref.name_span)),
        "PartialOrd" => Ok(struct_partial_ord_impl(sd, &trait_ref.name_span)),
        other => Err(Error {
            file: file.to_string(),
            message: format!(
                "cannot derive `{}`: only Copy, Clone, PartialEq, Eq, PartialOrd, Ord are supported",
                other
            ),
            span: trait_ref.name_span.copy(),
        }),
    }
}

fn synthesize_enum_impl(
    file: &str,
    ed: &EnumDef,
    trait_ref: &DeriveTrait,
) -> Result<ImplBlock, Error> {
    match trait_ref.name.as_str() {
        "Copy" => Ok(empty_marker_impl(
            &ed.lifetime_params,
            &ed.type_params,
            &ed.name,
            "Copy",
            &trait_ref.name_span,
        )),
        "Eq" => Ok(empty_marker_impl(
            &ed.lifetime_params,
            &ed.type_params,
            &ed.name,
            "Eq",
            &trait_ref.name_span,
        )),
        "Ord" => Ok(empty_marker_impl(
            &ed.lifetime_params,
            &ed.type_params,
            &ed.name,
            "Ord",
            &trait_ref.name_span,
        )),
        "Clone" => Ok(enum_clone_impl(ed, &trait_ref.name_span)),
        "PartialEq" => Ok(enum_partial_eq_impl(ed, &trait_ref.name_span)),
        "PartialOrd" => Ok(enum_partial_ord_impl(ed, &trait_ref.name_span)),
        other => Err(Error {
            file: file.to_string(),
            message: format!(
                "cannot derive `{}`: only Copy, Clone, PartialEq, Eq, PartialOrd, Ord are supported",
                other
            ),
            span: trait_ref.name_span.copy(),
        }),
    }
}

// =====================================================================
// Marker traits — empty impl block.
// =====================================================================

fn empty_marker_impl(
    target_lifetime_params: &Vec<LifetimeParam>,
    target_type_params: &Vec<TypeParam>,
    target_name: &str,
    trait_name: &str,
    span: &Span,
) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(target_type_params, target_lifetime_params, trait_name, span);
    let trait_path = mk_path(&[trait_name], Vec::new(), span);
    let target = target_type_for(target_name, target_type_params, span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: Vec::new(),
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

// =====================================================================
// Clone — struct
// =====================================================================

fn struct_clone_impl(sd: &StructDef, span: &Span) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(&sd.type_params, &sd.lifetime_params, "Clone", span);
    let trait_path = mk_path(&["Clone"], Vec::new(), span);
    let target = target_type_for(&sd.name, &sd.type_params, span);
    let return_type = self_target_type(&sd.name, &sd.type_params, span);
    let method = build_struct_clone_method(sd, return_type, span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: vec![method],
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

fn build_struct_clone_method(sd: &StructDef, return_type: Type, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let body = if sd.fields.is_empty() {
        // `S {}` — no fields to clone; build the empty struct literal.
        let path = mk_path(&[&sd.name], Vec::new(), span);
        let lit = b.struct_lit(path, Vec::new());
        b.block(Vec::new(), Some(lit))
    } else {
        // `S { f: self.f.clone(), ... }`. Method-dispatch autoref
        // takes care of the borrow on the receiver (Clone::clone has
        // a `&self`-shape, so recv_adjust resolves to BorrowImm).
        let path = mk_path(&[&sd.name], Vec::new(), span);
        let mut fields: Vec<(String, Expr)> = Vec::new();
        for f in &sd.fields {
            let self_v = b.var("self");
            let fa = b.field_access(self_v, &f.name);
            let cloned = b.method_call(fa, "clone", Vec::new());
            fields.push((f.name.clone(), cloned));
        }
        let lit = b.struct_lit(path, fields);
        b.block(Vec::new(), Some(lit))
    };
    Function {
        name: "clone".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![mk_self_param(span)],
        return_type: Some(return_type),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

// =====================================================================
// Clone — enum
// =====================================================================

fn enum_clone_impl(ed: &EnumDef, span: &Span) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(&ed.type_params, &ed.lifetime_params, "Clone", span);
    let trait_path = mk_path(&["Clone"], Vec::new(), span);
    let target = target_type_for(&ed.name, &ed.type_params, span);
    let return_type = self_target_type(&ed.name, &ed.type_params, span);
    let method = build_enum_clone_method(ed, return_type, span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: vec![method],
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

fn build_enum_clone_method(ed: &EnumDef, return_type: Type, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let mut arms: Vec<MatchArm> = Vec::new();
    for v in &ed.variants {
        let pat = build_variant_pattern_named(&mut b, &ed.name, v, "");
        let body = build_variant_clone_body(&mut b, &ed.name, v);
        arms.push(MatchArm {
            pattern: pat,
            guard: None,
            body,
            span: span.copy(),
        });
    }
    let self_v = b.var("self");
    let match_expr = b.match_expr(self_v, arms);
    let body = b.block(Vec::new(), Some(match_expr));
    Function {
        name: "clone".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![mk_self_param(span)],
        return_type: Some(return_type),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

// Build a variant pattern with bindings named `<base><field>` (or the
// field's actual name for struct variants) — bindings inherit
// match-ergonomics' Ref mode, so each becomes `&PayloadType`. `base`
// distinguishes scrutinees in two-level matches (e.g. PartialEq's
// `match self { ... } match other { ... }`); for single-match cases
// pass `""`.
fn build_variant_pattern_named(
    b: &mut Builder,
    enum_name: &str,
    v: &EnumVariant,
    base: &str,
) -> Pattern {
    let path = mk_path(&[enum_name, &v.name], Vec::new(), &b.s());
    match &v.payload {
        VariantPayload::Unit => b.variant_unit_pat(path),
        VariantPayload::Tuple(elems) => {
            let mut pats: Vec<Pattern> = Vec::new();
            for i in 0..elems.len() {
                let name = format!("_{}{}", base, i);
                pats.push(b.binding_pat(&name));
            }
            b.variant_tuple_pat(path, pats)
        }
        VariantPayload::Struct(fields) => {
            let mut field_pats: Vec<FieldPattern> = Vec::new();
            for f in fields {
                let bind_name = if base.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}_{}", f.name, base)
                };
                let p = b.binding_pat(&bind_name);
                field_pats.push(FieldPattern {
                    name: f.name.clone(),
                    name_span: b.s(),
                    pattern: p,
                });
            }
            b.variant_struct_pat(path, field_pats)
        }
    }
}

fn build_variant_clone_body(b: &mut Builder, enum_name: &str, v: &EnumVariant) -> Expr {
    let path = mk_path(&[enum_name, &v.name], Vec::new(), &b.s());
    match &v.payload {
        VariantPayload::Unit => {
            // Call form with empty args matches pocket-rust's
            // unit-variant construction (`Option::None`).
            b.variant_call(path, Vec::new())
        }
        VariantPayload::Tuple(elems) => {
            let mut args: Vec<Expr> = Vec::new();
            for i in 0..elems.len() {
                let name = format!("_{}", i);
                let v = b.var(&name);
                let cloned = b.method_call(v, "clone", Vec::new());
                args.push(cloned);
            }
            b.variant_call(path, args)
        }
        VariantPayload::Struct(fields) => {
            let mut field_inits: Vec<(String, Expr)> = Vec::new();
            for f in fields {
                let v = b.var(&f.name);
                let cloned = b.method_call(v, "clone", Vec::new());
                field_inits.push((f.name.clone(), cloned));
            }
            b.struct_lit(path, field_inits)
        }
    }
}

// =====================================================================
// PartialEq — struct
// =====================================================================

fn struct_partial_eq_impl(sd: &StructDef, span: &Span) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(&sd.type_params, &sd.lifetime_params, "PartialEq", span);
    let trait_path = mk_path(&["PartialEq"], Vec::new(), span);
    let target = target_type_for(&sd.name, &sd.type_params, span);
    let other_target = self_target_type(&sd.name, &sd.type_params, span);
    let eq = build_struct_eq_method(sd, &other_target, span);
    let ne = build_ne_method_from_eq(span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: vec![eq, ne],
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

fn build_struct_eq_method(sd: &StructDef, other_target: &Type, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let body_expr = if sd.fields.is_empty() {
        b.bool_lit(true)
    } else {
        // self.f0.eq(&other.f0) && self.f1.eq(&other.f1) && ...
        // The receiver autoref's via PartialEq::eq's BorrowImm shape;
        // the explicit `&` on `other.f` builds the `&Self` arg
        // PartialEq's signature requires.
        let mut acc: Option<Expr> = None;
        for f in &sd.fields {
            let self_v = b.var("self");
            let self_f = b.field_access(self_v, &f.name);
            let other_v = b.var("other");
            let other_f = b.field_access(other_v, &f.name);
            let other_borrow = b.borrow(other_f, false);
            let eq_call = b.method_call(self_f, "eq", vec![other_borrow]);
            acc = Some(match acc {
                None => eq_call,
                Some(prev) => build_short_circuit_and(&mut b, prev, eq_call),
            });
        }
        acc.unwrap()
    };
    let body = b.block(Vec::new(), Some(body_expr));
    Function {
        name: "eq".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![mk_self_param(span), mk_other_param(other_target, span)],
        return_type: Some(mk_bool_type(span)),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

// `fn ne(&self, other: &Self) -> bool { ¤bool_not(self.eq(other)) }`
fn build_ne_method_from_eq(span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let self_v = b.var("self");
    let other_v = b.var("other");
    let eq_call = b.method_call(self_v, "eq", vec![other_v]);
    let not = b.builtin("bool_not", vec![eq_call]);
    let body = b.block(Vec::new(), Some(not));
    Function {
        name: "ne".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![
            mk_self_param(span),
            Param {
                name: "other".to_string(),
                name_span: span.copy(),
                ty: mk_ref_type(mk_self_type(span), false),
            },
        ],
        return_type: Some(mk_bool_type(span)),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

// `lhs && rhs` desugars at parse time to `if lhs { rhs } else { false }`.
// Match that desugaring here so the synthesized AST has the same shape
// as user-written `&&`.
fn build_short_circuit_and(b: &mut Builder, lhs: Expr, rhs: Expr) -> Expr {
    let then_block = b.block(Vec::new(), Some(rhs));
    let false_lit = b.bool_lit(false);
    let else_block = b.block(Vec::new(), Some(false_lit));
    b.if_then_else(lhs, then_block, else_block)
}

// =====================================================================
// PartialEq — enum
// =====================================================================

fn enum_partial_eq_impl(ed: &EnumDef, span: &Span) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(&ed.type_params, &ed.lifetime_params, "PartialEq", span);
    let trait_path = mk_path(&["PartialEq"], Vec::new(), span);
    let target = target_type_for(&ed.name, &ed.type_params, span);
    let other_target = self_target_type(&ed.name, &ed.type_params, span);
    let eq = build_enum_eq_method(ed, &other_target, span);
    let ne = build_ne_method_from_eq(span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: vec![eq, ne],
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

fn build_enum_eq_method(ed: &EnumDef, other_target: &Type, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    // match self {
    //   <variant pattern (a-tagged)> => match other {
    //     <same variant pattern (b-tagged)> => <pairwise eq AND-chain>,
    //     _ => false,
    //   },
    //   ...
    // }
    let mut outer_arms: Vec<MatchArm> = Vec::new();
    for v in &ed.variants {
        let outer_pat = build_variant_pattern_named(&mut b, &ed.name, v, "a");
        // Inner match scrutinee: `other`.
        let mut inner_arms: Vec<MatchArm> = Vec::new();
        let inner_pat = build_variant_pattern_named(&mut b, &ed.name, v, "b");
        let inner_body = build_variant_eq_body(&mut b, v);
        inner_arms.push(MatchArm {
            pattern: inner_pat,
            guard: None,
            body: inner_body,
            span: span.copy(),
        });
        if ed.variants.len() > 1 {
            // Catch-all for the non-matching variants.
            let wild = b.wildcard_pat();
            let false_lit = b.bool_lit(false);
            inner_arms.push(MatchArm {
                pattern: wild,
                guard: None,
                body: false_lit,
                span: span.copy(),
            });
        }
        let other_v = b.var("other");
        let inner_match = b.match_expr(other_v, inner_arms);
        outer_arms.push(MatchArm {
            pattern: outer_pat,
            guard: None,
            body: inner_match,
            span: span.copy(),
        });
    }
    let self_v = b.var("self");
    let outer_match = b.match_expr(self_v, outer_arms);
    let body = b.block(Vec::new(), Some(outer_match));
    Function {
        name: "eq".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![mk_self_param(span), mk_other_param(other_target, span)],
        return_type: Some(mk_bool_type(span)),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

fn build_variant_eq_body(b: &mut Builder, v: &EnumVariant) -> Expr {
    match &v.payload {
        VariantPayload::Unit => b.bool_lit(true),
        VariantPayload::Tuple(elems) => {
            if elems.is_empty() {
                return b.bool_lit(true);
            }
            let mut acc: Option<Expr> = None;
            for i in 0..elems.len() {
                let a = b.var(&format!("_a{}", i));
                let bv = b.var(&format!("_b{}", i));
                let eq_call = b.method_call(a, "eq", vec![bv]);
                acc = Some(match acc {
                    None => eq_call,
                    Some(prev) => build_short_circuit_and(b, prev, eq_call),
                });
            }
            acc.unwrap()
        }
        VariantPayload::Struct(fields) => {
            if fields.is_empty() {
                return b.bool_lit(true);
            }
            let mut acc: Option<Expr> = None;
            for f in fields {
                let a = b.var(&format!("{}_a", f.name));
                let bv = b.var(&format!("{}_b", f.name));
                let eq_call = b.method_call(a, "eq", vec![bv]);
                acc = Some(match acc {
                    None => eq_call,
                    Some(prev) => build_short_circuit_and(b, prev, eq_call),
                });
            }
            acc.unwrap()
        }
    }
}

// =====================================================================
// PartialOrd — struct (lexicographic over fields)
// =====================================================================

fn struct_partial_ord_impl(sd: &StructDef, span: &Span) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(&sd.type_params, &sd.lifetime_params, "PartialOrd", span);
    let trait_path = mk_path(&["PartialOrd"], Vec::new(), span);
    let target = target_type_for(&sd.name, &sd.type_params, span);
    let other_target = self_target_type(&sd.name, &sd.type_params, span);
    let lt = build_struct_lt_method(sd, &other_target, span);
    let le = build_simple_partial_ord_companion("le", true, span);
    let gt = build_simple_partial_ord_companion("gt", false, span);
    let ge = build_simple_partial_ord_companion("ge", true, span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: vec![lt, le, gt, ge],
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

// Emit `if self.f.lt(&other.f) { return true; } if other.f.lt(&self.f) { return false; }`
// per field, then `false` at the tail.
fn build_struct_lt_method(sd: &StructDef, other_target: &Type, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let mut stmts: Vec<Stmt> = Vec::new();
    for f in &sd.fields {
        // Forward branch: self.f.lt(&other.f) → return true.
        {
            let self_v = b.var("self");
            let self_f = b.field_access(self_v, &f.name);
            let other_v = b.var("other");
            let other_f = b.field_access(other_v, &f.name);
            let other_borrow = b.borrow(other_f, false);
            let lt_call = b.method_call(self_f, "lt", vec![other_borrow]);
            let true_lit = b.bool_lit(true);
            let ret = b.return_expr(Some(true_lit));
            let then_block = b.block(vec![Stmt::Expr(ret)], None);
            let else_block = b.block(Vec::new(), None);
            let if_expr = b.if_then_else(lt_call, then_block, else_block);
            stmts.push(Stmt::Expr(if_expr));
        }
        // Backward branch: other.f.lt(&self.f) → return false.
        {
            let other_v = b.var("other");
            let other_f = b.field_access(other_v, &f.name);
            let self_v = b.var("self");
            let self_f = b.field_access(self_v, &f.name);
            let self_borrow = b.borrow(self_f, false);
            let lt_call = b.method_call(other_f, "lt", vec![self_borrow]);
            let false_lit = b.bool_lit(false);
            let ret = b.return_expr(Some(false_lit));
            let then_block = b.block(vec![Stmt::Expr(ret)], None);
            let else_block = b.block(Vec::new(), None);
            let if_expr = b.if_then_else(lt_call, then_block, else_block);
            stmts.push(Stmt::Expr(if_expr));
        }
    }
    let tail_false = b.bool_lit(false);
    let body = b.block(stmts, Some(tail_false));
    Function {
        name: "lt".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![mk_self_param(span), mk_other_param(other_target, span)],
        return_type: Some(mk_bool_type(span)),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

// =====================================================================
// PartialOrd — enum (variant-disc lex order, then payload lex order)
// =====================================================================
//
// `lt(self, other)` becomes a nested match: outer over `self`, inner
// over `other`. For variant pairs `(V_i, V_j)`:
//   - `i < j` → return `true`  (V_i is "smaller" by declaration order)
//   - `i > j` → return `false`
//   - `i == j` → recurse lexicographically through the variant's
//                payload (same shape as struct lt: per-position
//                forward/backward checks with early returns).
// `le`/`gt`/`ge` derive from `lt` exactly like the struct case.
fn enum_partial_ord_impl(ed: &EnumDef, span: &Span) -> ImplBlock {
    let (lifetime_params, type_params) =
        render_impl_generics(&ed.type_params, &ed.lifetime_params, "PartialOrd", span);
    let trait_path = mk_path(&["PartialOrd"], Vec::new(), span);
    let target = target_type_for(&ed.name, &ed.type_params, span);
    let other_target = self_target_type(&ed.name, &ed.type_params, span);
    let lt = build_enum_lt_method(ed, &other_target, span);
    let le = build_simple_partial_ord_companion("le", true, span);
    let gt = build_simple_partial_ord_companion("gt", false, span);
    let ge = build_simple_partial_ord_companion("ge", true, span);
    ImplBlock {
        lifetime_params,
        type_params,
        trait_path: Some(trait_path),
        target,
        methods: vec![lt, le, gt, ge],
        assoc_type_bindings: Vec::new(),
        span: span.copy(),
    }
}

fn build_enum_lt_method(ed: &EnumDef, other_target: &Type, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let mut outer_arms: Vec<MatchArm> = Vec::new();
    let n = ed.variants.len();
    let mut i = 0;
    while i < n {
        let vi = &ed.variants[i];
        let self_pat = build_variant_pattern_named(&mut b, &ed.name, vi, "a");
        let mut inner_arms: Vec<MatchArm> = Vec::new();
        let mut j = 0;
        while j < n {
            let vj = &ed.variants[j];
            let other_pat = build_variant_pattern_named(&mut b, &ed.name, vj, "b");
            let body = if i < j {
                b.bool_lit(true)
            } else if i > j {
                b.bool_lit(false)
            } else {
                build_variant_lt_body(&mut b, vi)
            };
            inner_arms.push(MatchArm {
                pattern: other_pat,
                guard: None,
                body,
                span: span.copy(),
            });
            j += 1;
        }
        let other_v = b.var("other");
        let inner_match = b.match_expr(other_v, inner_arms);
        outer_arms.push(MatchArm {
            pattern: self_pat,
            guard: None,
            body: inner_match,
            span: span.copy(),
        });
        i += 1;
    }
    let self_v = b.var("self");
    let outer_match = b.match_expr(self_v, outer_arms);
    let body = b.block(Vec::new(), Some(outer_match));
    Function {
        name: "lt".to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![mk_self_param(span), mk_other_param(other_target, span)],
        return_type: Some(mk_bool_type(span)),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

// For the same-variant arm: lexicographic comparison through payload
// fields. Same shape as `build_struct_lt_method`'s body — early
// `return true`/`return false` per field — wrapped in a Block expr so
// the arm body can carry multiple statements + a tail. Bindings come
// from `build_variant_pattern_named` (`_a0`/`_b0`/... for tuple
// variants, `<field>_a`/`<field>_b` for struct variants).
fn build_variant_lt_body(b: &mut Builder, v: &EnumVariant) -> Expr {
    match &v.payload {
        VariantPayload::Unit => b.bool_lit(false),
        VariantPayload::Tuple(elems) => {
            if elems.is_empty() {
                return b.bool_lit(false);
            }
            let mut stmts: Vec<Stmt> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                let a_name = format!("_a{}", i);
                let b_name = format!("_b{}", i);
                push_lt_pair_stmts(b, &mut stmts, &a_name, &b_name);
                i += 1;
            }
            let tail = b.bool_lit(false);
            let block = b.block(stmts, Some(tail));
            b.expr(ExprKind::Block(Box::new(block)))
        }
        VariantPayload::Struct(fields) => {
            if fields.is_empty() {
                return b.bool_lit(false);
            }
            let mut stmts: Vec<Stmt> = Vec::new();
            for f in fields {
                let a_name = format!("{}_a", f.name);
                let b_name = format!("{}_b", f.name);
                push_lt_pair_stmts(b, &mut stmts, &a_name, &b_name);
            }
            let tail = b.bool_lit(false);
            let block = b.block(stmts, Some(tail));
            b.expr(ExprKind::Block(Box::new(block)))
        }
    }
}

// Append the per-position lexicographic ordering pair:
//   if a.lt(b) { return true; }
//   if b.lt(a) { return false; }
// `a` and `b` are already `&T` bindings via match-ergonomics, so the
// `lt` call doesn't need an explicit `&` on the argument.
fn push_lt_pair_stmts(b: &mut Builder, stmts: &mut Vec<Stmt>, a_name: &str, b_name: &str) {
    {
        let av = b.var(a_name);
        let bv = b.var(b_name);
        let lt_call = b.method_call(av, "lt", vec![bv]);
        let true_lit = b.bool_lit(true);
        let ret = b.return_expr(Some(true_lit));
        let then_block = b.block(vec![Stmt::Expr(ret)], None);
        let else_block = b.block(Vec::new(), None);
        let if_expr = b.if_then_else(lt_call, then_block, else_block);
        stmts.push(Stmt::Expr(if_expr));
    }
    {
        let av = b.var(a_name);
        let bv = b.var(b_name);
        let lt_call = b.method_call(bv, "lt", vec![av]);
        let false_lit = b.bool_lit(false);
        let ret = b.return_expr(Some(false_lit));
        let then_block = b.block(vec![Stmt::Expr(ret)], None);
        let else_block = b.block(Vec::new(), None);
        let if_expr = b.if_then_else(lt_call, then_block, else_block);
        stmts.push(Stmt::Expr(if_expr));
    }
}

// `le`, `gt`, `ge` each derive from `lt`:
//   le: !(other.lt(self))
//   gt: other.lt(self)
//   ge: !(self.lt(other))
fn build_simple_partial_ord_companion(name: &str, negate: bool, span: &Span) -> Function {
    let mut b = Builder::new(span.copy());
    let (recv_name, arg_name) = match name {
        "le" => ("other", "self"),
        "gt" => ("other", "self"),
        "ge" => ("self", "other"),
        _ => unreachable!("unknown PartialOrd companion `{}`", name),
    };
    let recv = b.var(recv_name);
    let arg = b.var(arg_name);
    let lt_call = b.method_call(recv, "lt", vec![arg]);
    let body_expr = if negate {
        b.builtin("bool_not", vec![lt_call])
    } else {
        lt_call
    };
    let body = b.block(Vec::new(), Some(body_expr));
    Function {
        name: name.to_string(),
        name_span: span.copy(),
        lifetime_params: Vec::new(),
        type_params: Vec::new(),
        params: vec![
            mk_self_param(span),
            Param {
                name: "other".to_string(),
                name_span: span.copy(),
                ty: mk_ref_type(mk_self_type(span), false),
            },
        ],
        return_type: Some(mk_bool_type(span)),
        body,
        node_count: b.next_id,
        is_pub: false,
        is_unsafe: false,
    }
}

