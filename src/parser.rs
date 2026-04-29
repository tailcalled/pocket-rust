use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, FieldInit, Function, ImplBlock, LetStmt,
    Lifetime, LifetimeParam, MethodCall, Param, Path, PathSegment, Stmt, StructDef, StructField,
    StructLit, TraitBound, TraitDef, TraitMethodSig, Type, TypeKind, TypeParam,
};
use crate::lexer::{Token, TokenKind, token_kind_name};
use crate::span::{Error, Pos, Span};

pub enum RawItem {
    Function(Function),
    Struct(StructDef),
    ModDecl { name: String, name_span: Span },
    Impl(ImplBlock),
    Trait(TraitDef),
}

pub fn parse(file: &str, tokens: Vec<Token>) -> Result<Vec<RawItem>, Error> {
    let mut p = Parser {
        file: file.to_string(),
        tokens,
        pos: 0,
        next_node_id: 0,
    };
    p.parse_items()
}

struct Parser {
    file: String,
    tokens: Vec<Token>,
    pos: usize,
    // Resets at parse_function entry; captured as Function.node_count at exit.
    next_node_id: u32,
}

impl Parser {
    fn parse_items(&mut self) -> Result<Vec<RawItem>, Error> {
        let mut items = Vec::new();
        while self.pos < self.tokens.len() {
            items.push(self.parse_item()?);
        }
        Ok(items)
    }

    fn parse_item(&mut self) -> Result<RawItem, Error> {
        if self.peek_kind(&TokenKind::Mod) {
            self.parse_mod_decl()
        } else if self.peek_kind(&TokenKind::Fn) {
            Ok(RawItem::Function(self.parse_function()?))
        } else if self.peek_kind(&TokenKind::Struct) {
            Ok(RawItem::Struct(self.parse_struct_def()?))
        } else if self.peek_kind(&TokenKind::Impl) {
            Ok(RawItem::Impl(self.parse_impl_block()?))
        } else if self.peek_kind(&TokenKind::Trait) {
            Ok(RawItem::Trait(self.parse_trait_def()?))
        } else {
            Err(self.error_at_current("expected `fn`, `mod`, `struct`, `impl`, or `trait`"))
        }
    }

    fn parse_impl_block(&mut self) -> Result<ImplBlock, Error> {
        let impl_span = self.expect(&TokenKind::Impl, "`impl`")?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        // First, peek to distinguish `impl Trait for Target` from `impl Target`.
        // We parse the first thing as a Type for the relaxed target case
        // (`impl Show for &T`); if `for` follows, that Type was actually the
        // trait — but trait paths are simpler (no `&`/`*`), so for `impl
        // Trait for Target` we parse the trait as a path and the target as a
        // full Type. We always try `Type` first, then if it's a Path-shaped
        // type and `for` follows, treat as trait impl.
        let first_type = self.parse_type()?;
        let (trait_path, target) = if self.peek_kind(&TokenKind::For) {
            self.pos += 1;
            // first_type must have been a Path (a trait reference). Extract.
            let trait_path = match first_type.kind {
                TypeKind::Path(p) => p,
                _ => {
                    return Err(Error {
                        file: self.file.clone(),
                        message: "expected trait path before `for`".to_string(),
                        span: first_type.span.copy(),
                    });
                }
            };
            let target = self.parse_type()?;
            (Some(trait_path), target)
        } else {
            (None, first_type)
        };
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut methods: Vec<Function> = Vec::new();
        while !self.peek_kind(&TokenKind::RBrace) {
            if !self.peek_kind(&TokenKind::Fn) {
                return Err(self.error_at_current("expected `fn` inside `impl` block"));
            }
            methods.push(self.parse_function()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(ImplBlock {
            lifetime_params,
            type_params,
            trait_path,
            target,
            methods,
            span: Span::new(impl_span.start, rb.end),
        })
    }

    fn parse_trait_def(&mut self) -> Result<TraitDef, Error> {
        let trait_kw = self.expect(&TokenKind::Trait, "`trait`")?;
        let (name, name_span) = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut methods: Vec<TraitMethodSig> = Vec::new();
        while !self.peek_kind(&TokenKind::RBrace) {
            if !self.peek_kind(&TokenKind::Fn) {
                return Err(self.error_at_current("expected `fn` inside `trait` body"));
            }
            methods.push(self.parse_trait_method_sig()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(TraitDef {
            name,
            name_span,
            methods,
            span: Span::new(trait_kw.start, rb.end),
        })
    }

    // Trait method signature: same shape as `parse_function` but ends in `;`
    // (no body). Receiver shorthand allowed (`&self`, `&mut self`, `self`).
    fn parse_trait_method_sig(&mut self) -> Result<TraitMethodSig, Error> {
        self.expect(&TokenKind::Fn, "`fn`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::LParen, "`(`")?;
        let params = if self.peek_kind(&TokenKind::RParen) {
            Vec::new()
        } else {
            self.parse_params()?
        };
        self.expect(&TokenKind::RParen, "`)`")?;
        let return_type = if self.peek_kind(&TokenKind::Arrow) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(TraitMethodSig {
            name,
            name_span,
            lifetime_params,
            type_params,
            params,
            return_type,
        })
    }

    // Parse a path that may carry generic args on its last segment using the
    // type-position syntax: `Foo<T1, T2>` (no `::`). Used for type references
    // and for impl targets.
    fn parse_path_with_type_args(&mut self) -> Result<Path, Error> {
        let mut path = self.parse_path()?;
        if self.peek_kind(&TokenKind::LAngle) {
            let (lifetime_args, args) = self.parse_angle_args()?;
            // Attach to the last segment.
            let last = path.segments.len() - 1;
            path.segments[last].lifetime_args = lifetime_args;
            path.segments[last].args = args;
            // Extend the path's span to cover the args.
            if let Some(end_pos) = self.tokens.get(self.pos.saturating_sub(1)) {
                path.span = Span::new(path.span.start.copy(), end_pos.span.end.copy());
            }
        }
        Ok(path)
    }

    // Parse `<'a, 'b, T1, T2>` — lifetime args first (Rust convention),
    // then type args. Either list may be empty. Used for type-position
    // generic args, intermediate path turbofish, and method-call turbofish.
    fn parse_angle_args(&mut self) -> Result<(Vec<Lifetime>, Vec<Type>), Error> {
        self.expect(&TokenKind::LAngle, "`<`")?;
        let mut lifetime_args: Vec<Lifetime> = Vec::new();
        let mut args: Vec<Type> = Vec::new();
        if !self.peek_kind(&TokenKind::RAngle) {
            // Lifetime args run first, while we still see `'name` tokens.
            while self.peek_lifetime() {
                let (name, span) = self.expect_lifetime()?;
                lifetime_args.push(Lifetime { name, span });
                if !self.peek_kind(&TokenKind::Comma) {
                    break;
                }
                self.pos += 1;
                if self.peek_kind(&TokenKind::RAngle) {
                    break;
                }
            }
            // Then type args.
            if !self.peek_kind(&TokenKind::RAngle) {
                args.push(self.parse_type()?);
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RAngle) {
                        break;
                    }
                    args.push(self.parse_type()?);
                }
            }
        }
        self.expect(&TokenKind::RAngle, "`>`")?;
        Ok((lifetime_args, args))
    }

    fn parse_mod_decl(&mut self) -> Result<RawItem, Error> {
        self.expect(&TokenKind::Mod, "`mod`")?;
        let (name, name_span) = self.expect_ident()?;
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(RawItem::ModDecl { name, name_span })
    }

    fn parse_struct_def(&mut self) -> Result<StructDef, Error> {
        self.expect(&TokenKind::Struct, "`struct`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        if !self.peek_kind(&TokenKind::RBrace) {
            fields.push(self.parse_struct_field()?);
            while self.peek_kind(&TokenKind::Comma) {
                self.pos += 1;
                if self.peek_kind(&TokenKind::RBrace) {
                    break;
                }
                fields.push(self.parse_struct_field()?);
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(StructDef {
            name,
            name_span,
            lifetime_params,
            type_params,
            fields,
        })
    }

    fn parse_struct_field(&mut self) -> Result<StructField, Error> {
        let (name, name_span) = self.expect_ident()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        Ok(StructField {
            name,
            name_span,
            ty,
        })
    }

    fn parse_function(&mut self) -> Result<Function, Error> {
        self.expect(&TokenKind::Fn, "`fn`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::LParen, "`(`")?;
        let params = if self.peek_kind(&TokenKind::RParen) {
            Vec::new()
        } else {
            self.parse_params()?
        };
        self.expect(&TokenKind::RParen, "`)`")?;
        let return_type = if self.peek_kind(&TokenKind::Arrow) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        // Reset per-function NodeId counter; capture node_count at exit.
        let saved_node_id = self.next_node_id;
        self.next_node_id = 0;
        let body = self.parse_block()?;
        let node_count = self.next_node_id;
        self.next_node_id = saved_node_id;
        Ok(Function {
            name,
            name_span,
            lifetime_params,
            type_params,
            params,
            return_type,
            body,
            node_count,
        })
    }

    fn alloc_node_id(&mut self) -> crate::ast::NodeId {
        let id = self.next_node_id;
        self.next_node_id += 1;
        id
    }

    // Parse a generic-params list `<'a, 'b, T1, T2>`. Lifetime params come
    // first (Rust convention); a lifetime after a type param is rejected.
    fn parse_generic_params(&mut self) -> Result<(Vec<LifetimeParam>, Vec<TypeParam>), Error> {
        self.expect(&TokenKind::LAngle, "`<`")?;
        let mut lifetime_params: Vec<LifetimeParam> = Vec::new();
        let mut type_params: Vec<TypeParam> = Vec::new();
        if !self.peek_kind(&TokenKind::RAngle) {
            // Lifetime params, while we still see `'name`.
            while self.peek_lifetime() {
                let (name, name_span) = self.expect_lifetime()?;
                lifetime_params.push(LifetimeParam { name, name_span });
                if !self.peek_kind(&TokenKind::Comma) {
                    break;
                }
                self.pos += 1;
                if self.peek_kind(&TokenKind::RAngle) {
                    break;
                }
            }
            // Then type params; lifetimes interleaved here are rejected.
            if !self.peek_kind(&TokenKind::RAngle) {
                if self.peek_lifetime() {
                    let span = self.tokens[self.pos].span.copy();
                    return Err(Error {
                        file: self.file.clone(),
                        message: "lifetime parameters must come before type parameters"
                            .to_string(),
                        span,
                    });
                }
                type_params.push(self.parse_type_param()?);
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RAngle) {
                        break;
                    }
                    if self.peek_lifetime() {
                        let span = self.tokens[self.pos].span.copy();
                        return Err(Error {
                            file: self.file.clone(),
                            message: "lifetime parameters must come before type parameters"
                                .to_string(),
                            span,
                        });
                    }
                    type_params.push(self.parse_type_param()?);
                }
            }
        }
        self.expect(&TokenKind::RAngle, "`>`")?;
        Ok((lifetime_params, type_params))
    }

    fn parse_type_param(&mut self) -> Result<TypeParam, Error> {
        let (name, name_span) = self.expect_ident()?;
        let mut bounds: Vec<TraitBound> = Vec::new();
        if self.peek_kind(&TokenKind::Colon) {
            self.pos += 1;
            // First bound is required if `:` was present.
            bounds.push(self.parse_trait_bound()?);
            while self.peek_kind(&TokenKind::Plus) {
                self.pos += 1;
                bounds.push(self.parse_trait_bound()?);
            }
        }
        Ok(TypeParam { name, name_span, bounds })
    }

    fn parse_trait_bound(&mut self) -> Result<TraitBound, Error> {
        // For now: a bound is just a path (e.g. `Show` or `crate::Foo`). Trait
        // generic args (e.g. `Iterator<Item>`) aren't supported yet — we only
        // recognize path-shaped bounds.
        let path = self.parse_path_with_type_args()?;
        Ok(TraitBound { path })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, Error> {
        let mut params = Vec::new();
        if let Some(recv) = self.try_parse_receiver()? {
            params.push(recv);
        } else {
            params.push(self.parse_param()?);
        }
        while self.peek_kind(&TokenKind::Comma) {
            self.pos += 1;
            if self.peek_kind(&TokenKind::RParen) {
                break;
            }
            params.push(self.parse_param()?);
        }
        Ok(params)
    }

    // Receiver shorthand: `self` / `&self` / `&mut self`. Desugared to a
    // regular `self: Self` / `self: &Self` / `self: &mut Self` param. Only
    // valid as the first param (caller restricts).
    fn try_parse_receiver(&mut self) -> Result<Option<Param>, Error> {
        if self.peek_kind(&TokenKind::SelfLower) {
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            return Ok(Some(Param {
                name: "self".to_string(),
                name_span: span.copy(),
                ty: Type {
                    kind: TypeKind::SelfType,
                    span,
                },
            }));
        }
        if self.peek_kind(&TokenKind::Amp) {
            let save_pos = self.pos;
            let amp_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            // Optional lifetime annotation: `&'a self` / `&'a mut self`.
            let lifetime = if self.peek_lifetime() {
                let (name, span) = self.expect_lifetime()?;
                Some(Lifetime { name, span })
            } else {
                None
            };
            let mutable = if self.peek_kind(&TokenKind::Mut) {
                self.pos += 1;
                true
            } else {
                false
            };
            if self.peek_kind(&TokenKind::SelfLower) {
                let self_span = self.tokens[self.pos].span.copy();
                let outer = Span::new(amp_span.start.copy(), self_span.end.copy());
                self.pos += 1;
                let inner = Type {
                    kind: TypeKind::SelfType,
                    span: self_span.copy(),
                };
                return Ok(Some(Param {
                    name: "self".to_string(),
                    name_span: self_span,
                    ty: Type {
                        kind: TypeKind::Ref {
                            inner: Box::new(inner),
                            mutable,
                            lifetime,
                        },
                        span: outer,
                    },
                }));
            }
            // Not a receiver — backtrack so parse_param sees the original `&`.
            self.pos = save_pos;
        }
        Ok(None)
    }

    fn parse_param(&mut self) -> Result<Param, Error> {
        let (name, name_span) = self.expect_ident()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        Ok(Param {
            name,
            name_span,
            ty,
        })
    }

    fn parse_type(&mut self) -> Result<Type, Error> {
        if self.peek_kind(&TokenKind::SelfUpper) {
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            return Ok(Type {
                kind: TypeKind::SelfType,
                span,
            });
        }
        if self.peek_kind(&TokenKind::Amp) {
            let amp_span = self.expect(&TokenKind::Amp, "`&`")?;
            let lifetime = if self.peek_lifetime() {
                let (name, span) = self.expect_lifetime()?;
                Some(Lifetime { name, span })
            } else {
                None
            };
            let mutable = if self.peek_kind(&TokenKind::Mut) {
                self.pos += 1;
                true
            } else {
                false
            };
            let inner = self.parse_type()?;
            let span = Span::new(amp_span.start, inner.span.end.copy());
            return Ok(Type {
                kind: TypeKind::Ref {
                    inner: Box::new(inner),
                    mutable,
                    lifetime,
                },
                span,
            });
        }
        if self.peek_kind(&TokenKind::Star) {
            let star_span = self.expect(&TokenKind::Star, "`*`")?;
            let mutable = if self.peek_kind(&TokenKind::Mut) {
                self.pos += 1;
                true
            } else if self.peek_kind(&TokenKind::Const) {
                self.pos += 1;
                false
            } else {
                return Err(Error {
                    file: self.file.clone(),
                    message: "expected `const` or `mut` after `*` in pointer type".to_string(),
                    span: star_span,
                });
            };
            let inner = self.parse_type()?;
            let span = Span::new(star_span.start, inner.span.end.copy());
            return Ok(Type {
                kind: TypeKind::RawPtr {
                    inner: Box::new(inner),
                    mutable,
                },
                span,
            });
        }
        let path = self.parse_path_with_type_args()?;
        let span = path.span.copy();
        Ok(Type {
            kind: TypeKind::Path(path),
            span,
        })
    }

    fn parse_block(&mut self) -> Result<Block, Error> {
        let lb = self.expect(&TokenKind::LBrace, "`{`")?;
        let mut stmts: Vec<Stmt> = Vec::new();
        let tail;
        loop {
            if self.peek_kind(&TokenKind::Let) {
                stmts.push(self.parse_let_stmt()?);
                continue;
            }
            if self.peek_kind(&TokenKind::RBrace) {
                tail = None;
                break;
            }
            let expr = self.parse_expr()?;
            if self.peek_kind(&TokenKind::Eq) {
                self.pos += 1;
                let rhs = self.parse_expr()?;
                let semi = self.expect(&TokenKind::Semi, "`;`")?;
                let span = Span::new(expr.span.start.copy(), semi.end);
                stmts.push(Stmt::Assign(AssignStmt {
                    lhs: expr,
                    rhs,
                    span,
                }));
                continue;
            }
            // Block-like expressions (`unsafe { … }` and `{ … }`) without a
            // tail can sit as bare statements with no trailing `;`. They're
            // unit-typed; we simply walk them for side effects in later passes.
            if is_unit_block_like(&expr) {
                // Block-like exprs without a tail are unit-typed; pocket-
                // rust has no unit value, so treat them as statements
                // even when they sit at the very end of the enclosing
                // block (which means the block has no tail).
                stmts.push(Stmt::Expr(expr));
                if self.peek_kind(&TokenKind::RBrace) {
                    tail = None;
                    break;
                }
                continue;
            }
            tail = Some(expr);
            break;
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Block {
            stmts,
            tail,
            span: Span::new(lb.start, rb.end),
        })
    }

    fn parse_let_stmt(&mut self) -> Result<Stmt, Error> {
        self.expect(&TokenKind::Let, "`let`")?;
        let mutable = if self.peek_kind(&TokenKind::Mut) {
            self.pos += 1;
            true
        } else {
            false
        };
        let (name, name_span) = self.expect_ident()?;
        let ty = if self.peek_kind(&TokenKind::Colon) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(Stmt::Let(LetStmt {
            name,
            name_span,
            mutable,
            ty,
            value,
        }))
    }

    fn parse_expr(&mut self) -> Result<Expr, Error> {
        self.parse_cast()
    }

    fn parse_cast(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_unary()?;
        while self.peek_kind(&TokenKind::As) {
            self.pos += 1;
            let ty = self.parse_type()?;
            let span = Span::new(expr.span.start.copy(), ty.span.end.copy());
            let id = self.alloc_node_id();
            expr = Expr {
                kind: ExprKind::Cast {
                    inner: Box::new(expr),
                    ty,
                },
                span,
                id,
            };
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, Error> {
        if self.peek_kind(&TokenKind::Amp) {
            let amp_span = self.expect(&TokenKind::Amp, "`&`")?;
            let mutable = if self.peek_kind(&TokenKind::Mut) {
                self.pos += 1;
                true
            } else {
                false
            };
            let inner = self.parse_unary()?;
            let span = Span::new(amp_span.start, inner.span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Expr {
                kind: ExprKind::Borrow {
                    inner: Box::new(inner),
                    mutable,
                },
                span,
                id,
            });
        }
        if self.peek_kind(&TokenKind::Star) {
            let star_span = self.expect(&TokenKind::Star, "`*`")?;
            let inner = self.parse_unary()?;
            let span = Span::new(star_span.start, inner.span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Expr {
                kind: ExprKind::Deref(Box::new(inner)),
                span,
                id,
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_atom()?;
        while self.peek_kind(&TokenKind::Dot) {
            self.pos += 1;
            let (field, field_span) = self.expect_ident()?;
            // Optional method-call turbofish: `.method::<'a, T>(args)`.
            // Lifetime args are parsed but ignored (Phase A).
            let turbofish_args = if self.peek_two(&TokenKind::PathSep, &TokenKind::LAngle) {
                self.pos += 1; // skip `::`
                let (_lifetime_args, args) = self.parse_angle_args()?;
                args
            } else {
                Vec::new()
            };
            if self.peek_kind(&TokenKind::LParen) {
                let args = self.parse_call_args()?;
                let end = self.tokens[self.pos - 1].span.end.copy();
                let span = Span::new(expr.span.start.copy(), end);
                let id = self.alloc_node_id();
                expr = Expr {
                    kind: ExprKind::MethodCall(MethodCall {
                        receiver: Box::new(expr),
                        method: field,
                        method_span: field_span,
                        turbofish_args,
                        args,
                    }),
                    span,
                    id,
                };
            } else if !turbofish_args.is_empty() {
                return Err(Error {
                    file: self.file.clone(),
                    message: "expected `(` after method-call turbofish `::<…>`".to_string(),
                    span: field_span,
                });
            } else {
                let span = Span::new(expr.span.start.copy(), field_span.end.copy());
                let id = self.alloc_node_id();
                expr = Expr {
                    kind: ExprKind::FieldAccess(FieldAccess {
                        base: Box::new(expr),
                        field,
                        field_span,
                    }),
                    span,
                    id,
                };
            }
        }
        Ok(expr)
    }

    fn parse_atom(&mut self) -> Result<Expr, Error> {
        if self.pos >= self.tokens.len() {
            return Err(Error {
                file: self.file.clone(),
                message: "expected expression, got end of input".to_string(),
                span: self.eof_span(),
            });
        }
        match &self.tokens[self.pos].kind {
            TokenKind::IntLit(_) => self.parse_int_lit(),
            TokenKind::Ident(_) => self.parse_path_atom(),
            TokenKind::SelfUpper => self.parse_path_atom(),
            TokenKind::SelfLower => {
                let span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let id = self.alloc_node_id();
                Ok(Expr {
                    kind: ExprKind::Var("self".to_string()),
                    span,
                    id,
                })
            }
            TokenKind::LBrace => self.parse_block_expr(),
            TokenKind::Unsafe => self.parse_unsafe_block(),
            TokenKind::LParen => {
                self.pos += 1;
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                Ok(expr)
            }
            other => {
                let msg = format!("expected expression, got {}", token_kind_name(other));
                let span = self.tokens[self.pos].span.copy();
                Err(Error {
                    file: self.file.clone(),
                    message: msg,
                    span,
                })
            }
        }
    }

    fn parse_block_expr(&mut self) -> Result<Expr, Error> {
        let block = self.parse_block()?;
        let span = block.span.copy();
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Block(Box::new(block)),
            span,
            id,
        })
    }

    // Recognize "block-like, no tail" expressions that can sit as a statement
    // in a block without a trailing `;`. Right now that's just `unsafe { … }`
    // and `{ … }` — both with `tail.is_none()`.


    fn parse_unsafe_block(&mut self) -> Result<Expr, Error> {
        let unsafe_span = self.expect(&TokenKind::Unsafe, "`unsafe`")?;
        let block = self.parse_block()?;
        let span = Span::new(unsafe_span.start, block.span.end.copy());
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Unsafe(Box::new(block)),
            span,
            id,
        })
    }

    fn parse_int_lit(&mut self) -> Result<Expr, Error> {
        let tok = &self.tokens[self.pos];
        match &tok.kind {
            TokenKind::IntLit(n) => {
                let value = *n;
                let span = tok.span.copy();
                self.pos += 1;
                let id = self.alloc_node_id();
                Ok(Expr {
                    kind: ExprKind::IntLit(value),
                    span,
                    id,
                })
            }
            _ => unreachable!(),
        }
    }

    fn parse_path_atom(&mut self) -> Result<Expr, Error> {
        let path = self.parse_path()?;
        let had_turbofish = path.segments.iter().any(|s| !s.args.is_empty());
        if self.peek_kind(&TokenKind::LParen) {
            let args = self.parse_call_args()?;
            let end = self.tokens[self.pos - 1].span.end.copy();
            let span = Span::new(path.span.start.copy(), end);
            let id = self.alloc_node_id();
            Ok(Expr {
                kind: ExprKind::Call(Call { callee: path, args }),
                span,
                id,
            })
        } else if self.peek_kind(&TokenKind::LBrace) {
            let fields = self.parse_struct_init()?;
            let end = self.tokens[self.pos - 1].span.end.copy();
            let span = Span::new(path.span.start.copy(), end);
            let id = self.alloc_node_id();
            Ok(Expr {
                kind: ExprKind::StructLit(StructLit { path, fields }),
                span,
                id,
            })
        } else if had_turbofish {
            Err(Error {
                file: self.file.clone(),
                message: "expected `(` or `{` after turbofish `::<…>`".to_string(),
                span: path.span.copy(),
            })
        } else if path.segments.len() == 1 {
            let seg = &path.segments[0];
            let id = self.alloc_node_id();
            Ok(Expr {
                kind: ExprKind::Var(seg.name.clone()),
                span: seg.span.copy(),
                id,
            })
        } else {
            Err(Error {
                file: self.file.clone(),
                message: "expected `(` or `{` after multi-segment path".to_string(),
                span: path.span.copy(),
            })
        }
    }

    fn peek_two(&self, a: &TokenKind, b: &TokenKind) -> bool {
        self.pos + 1 < self.tokens.len()
            && Self::kind_eq(&self.tokens[self.pos].kind, a)
            && Self::kind_eq(&self.tokens[self.pos + 1].kind, b)
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, Error> {
        self.expect(&TokenKind::LParen, "`(`")?;
        if self.peek_kind(&TokenKind::RParen) {
            self.pos += 1;
            return Ok(Vec::new());
        }
        let mut args = Vec::new();
        args.push(self.parse_expr()?);
        while self.peek_kind(&TokenKind::Comma) {
            self.pos += 1;
            if self.peek_kind(&TokenKind::RParen) {
                break;
            }
            args.push(self.parse_expr()?);
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Ok(args)
    }

    fn parse_struct_init(&mut self) -> Result<Vec<FieldInit>, Error> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        if !self.peek_kind(&TokenKind::RBrace) {
            fields.push(self.parse_field_init()?);
            while self.peek_kind(&TokenKind::Comma) {
                self.pos += 1;
                if self.peek_kind(&TokenKind::RBrace) {
                    break;
                }
                fields.push(self.parse_field_init()?);
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(fields)
    }

    fn parse_field_init(&mut self) -> Result<FieldInit, Error> {
        let (name, name_span) = self.expect_ident()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let value = self.parse_expr()?;
        Ok(FieldInit {
            name,
            name_span,
            value,
        })
    }

    fn parse_path(&mut self) -> Result<Path, Error> {
        let (first_name, first_span) = self.expect_path_segment()?;
        let start = first_span.start.copy();
        let mut end = first_span.end.copy();
        let mut segments = Vec::new();
        segments.push(PathSegment {
            name: first_name,
            span: first_span,
            lifetime_args: Vec::new(),
            args: Vec::new(),
        });
        loop {
            // Intermediate turbofish: `Pair::<'a, T>::method` — args attach
            // to the immediately preceding segment, then we keep parsing.
            if self.peek_two(&TokenKind::PathSep, &TokenKind::LAngle) {
                self.pos += 1; // skip `::`
                let (lifetime_args, args) = self.parse_angle_args()?;
                let last = segments.len() - 1;
                segments[last].lifetime_args = lifetime_args;
                segments[last].args = args;
                if self.pos > 0 {
                    end = self.tokens[self.pos - 1].span.end.copy();
                }
                continue;
            }
            // Plain `::name` — next segment.
            if self.peek_kind(&TokenKind::PathSep) {
                self.pos += 1;
                let (name, span) = self.expect_path_segment()?;
                end = span.end.copy();
                segments.push(PathSegment {
                    name,
                    span,
                    lifetime_args: Vec::new(),
                    args: Vec::new(),
                });
                continue;
            }
            break;
        }
        Ok(Path {
            segments,
            span: Span::new(start, end),
        })
    }

    // Like expect_ident, but also accepts `Self` as a path segment named "Self".
    fn expect_path_segment(&mut self) -> Result<(String, Span), Error> {
        if self.pos < self.tokens.len() {
            if let TokenKind::SelfUpper = &self.tokens[self.pos].kind {
                let span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                return Ok(("Self".to_string(), span));
            }
        }
        self.expect_ident()
    }

    fn expect(&mut self, kind: &TokenKind, label: &str) -> Result<Span, Error> {
        if self.pos >= self.tokens.len() {
            return Err(Error {
                file: self.file.clone(),
                message: format!("expected {}, got end of input", label),
                span: self.eof_span(),
            });
        }
        if Self::kind_eq(&self.tokens[self.pos].kind, kind) {
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            Ok(span)
        } else {
            let msg = format!(
                "expected {}, got {}",
                label,
                token_kind_name(&self.tokens[self.pos].kind)
            );
            let span = self.tokens[self.pos].span.copy();
            Err(Error {
                file: self.file.clone(),
                message: msg,
                span,
            })
        }
    }

    fn peek_lifetime(&self) -> bool {
        if self.pos >= self.tokens.len() {
            return false;
        }
        matches!(self.tokens[self.pos].kind, TokenKind::Lifetime(_))
    }

    fn expect_lifetime(&mut self) -> Result<(String, Span), Error> {
        if self.pos >= self.tokens.len() {
            return Err(Error {
                file: self.file.clone(),
                message: "expected lifetime, got end of input".to_string(),
                span: self.eof_span(),
            });
        }
        let span = self.tokens[self.pos].span.copy();
        match &self.tokens[self.pos].kind {
            TokenKind::Lifetime(s) => {
                let name = s.clone();
                self.pos += 1;
                Ok((name, span))
            }
            other => {
                let msg = format!("expected lifetime, got {}", token_kind_name(other));
                Err(Error {
                    file: self.file.clone(),
                    message: msg,
                    span,
                })
            }
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span), Error> {
        if self.pos >= self.tokens.len() {
            return Err(Error {
                file: self.file.clone(),
                message: "expected identifier, got end of input".to_string(),
                span: self.eof_span(),
            });
        }
        let span = self.tokens[self.pos].span.copy();
        match &self.tokens[self.pos].kind {
            TokenKind::Ident(s) => {
                let name = s.clone();
                self.pos += 1;
                Ok((name, span))
            }
            other => {
                let msg = format!("expected identifier, got {}", token_kind_name(other));
                Err(Error {
                    file: self.file.clone(),
                    message: msg,
                    span,
                })
            }
        }
    }

    fn peek_kind(&self, kind: &TokenKind) -> bool {
        self.pos < self.tokens.len() && Self::kind_eq(&self.tokens[self.pos].kind, kind)
    }

    fn kind_eq(a: &TokenKind, b: &TokenKind) -> bool {
        match (a, b) {
            (TokenKind::Fn, TokenKind::Fn) => true,
            (TokenKind::Mod, TokenKind::Mod) => true,
            (TokenKind::Struct, TokenKind::Struct) => true,
            (TokenKind::LParen, TokenKind::LParen) => true,
            (TokenKind::RParen, TokenKind::RParen) => true,
            (TokenKind::LBrace, TokenKind::LBrace) => true,
            (TokenKind::RBrace, TokenKind::RBrace) => true,
            (TokenKind::Arrow, TokenKind::Arrow) => true,
            (TokenKind::Semi, TokenKind::Semi) => true,
            (TokenKind::Colon, TokenKind::Colon) => true,
            (TokenKind::Dot, TokenKind::Dot) => true,
            (TokenKind::PathSep, TokenKind::PathSep) => true,
            (TokenKind::Comma, TokenKind::Comma) => true,
            (TokenKind::Amp, TokenKind::Amp) => true,
            (TokenKind::Star, TokenKind::Star) => true,
            (TokenKind::Let, TokenKind::Let) => true,
            (TokenKind::Mut, TokenKind::Mut) => true,
            (TokenKind::Const, TokenKind::Const) => true,
            (TokenKind::As, TokenKind::As) => true,
            (TokenKind::Unsafe, TokenKind::Unsafe) => true,
            (TokenKind::Impl, TokenKind::Impl) => true,
            (TokenKind::Trait, TokenKind::Trait) => true,
            (TokenKind::For, TokenKind::For) => true,
            (TokenKind::Plus, TokenKind::Plus) => true,
            (TokenKind::SelfLower, TokenKind::SelfLower) => true,
            (TokenKind::SelfUpper, TokenKind::SelfUpper) => true,
            (TokenKind::LAngle, TokenKind::LAngle) => true,
            (TokenKind::RAngle, TokenKind::RAngle) => true,
            (TokenKind::Eq, TokenKind::Eq) => true,
            _ => false,
        }
    }

    fn eof_span(&self) -> Span {
        if self.tokens.is_empty() {
            let p = Pos::new(1, 1);
            Span::new(p.copy(), p)
        } else {
            let last_end = self.tokens[self.tokens.len() - 1].span.end.copy();
            Span::new(last_end.copy(), last_end)
        }
    }

    fn error_at_current(&self, message: &str) -> Error {
        let span = if self.pos < self.tokens.len() {
            self.tokens[self.pos].span.copy()
        } else {
            self.eof_span()
        };
        Error {
            file: self.file.clone(),
            message: message.to_string(),
            span,
        }
    }
}

fn is_unit_block_like(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Block(b) => b.tail.is_none(),
        ExprKind::Unsafe(b) => b.tail.is_none(),
        _ => false,
    }
}
