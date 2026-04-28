use crate::ast::{
    Block, Call, Expr, ExprKind, FieldAccess, FieldInit, Function, Param, Path, PathSegment,
    StructDef, StructField, StructLit, Type, TypeKind,
};
use crate::lexer::{Token, TokenKind, token_kind_name};
use crate::span::{Error, Pos, Span};

pub enum RawItem {
    Function(Function),
    Struct(StructDef),
    ModDecl { name: String, name_span: Span },
}

pub fn parse(file: &str, tokens: Vec<Token>) -> Result<Vec<RawItem>, Error> {
    let mut p = Parser {
        file: file.to_string(),
        tokens,
        pos: 0,
    };
    p.parse_items()
}

struct Parser {
    file: String,
    tokens: Vec<Token>,
    pos: usize,
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
        } else {
            Err(self.error_at_current("expected `fn`, `mod`, or `struct`"))
        }
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
        let body = self.parse_block()?;
        Ok(Function {
            name,
            name_span,
            params,
            return_type,
            body,
        })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, Error> {
        let mut params = Vec::new();
        params.push(self.parse_param()?);
        while self.peek_kind(&TokenKind::Comma) {
            self.pos += 1;
            if self.peek_kind(&TokenKind::RParen) {
                break;
            }
            params.push(self.parse_param()?);
        }
        Ok(params)
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
        if self.peek_kind(&TokenKind::Amp) {
            let amp_span = self.expect(&TokenKind::Amp, "`&`")?;
            let inner = self.parse_type()?;
            let span = Span::new(amp_span.start, inner.span.end.copy());
            return Ok(Type {
                kind: TypeKind::Ref(Box::new(inner)),
                span,
            });
        }
        let path = self.parse_path()?;
        if path.segments.len() == 1 && path.segments[0].name == "usize" {
            Ok(Type {
                kind: TypeKind::Usize,
                span: path.span,
            })
        } else {
            let span = path.span.copy();
            Ok(Type {
                kind: TypeKind::Struct(path),
                span,
            })
        }
    }

    fn parse_block(&mut self) -> Result<Block, Error> {
        let lb = self.expect(&TokenKind::LBrace, "`{`")?;
        let tail = if self.peek_kind(&TokenKind::RBrace) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Block {
            tail,
            span: Span::new(lb.start, rb.end),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, Error> {
        self.parse_unary()
    }

    fn parse_unary(&mut self) -> Result<Expr, Error> {
        if self.peek_kind(&TokenKind::Amp) {
            let amp_span = self.expect(&TokenKind::Amp, "`&`")?;
            let inner = self.parse_unary()?;
            let span = Span::new(amp_span.start, inner.span.end.copy());
            return Ok(Expr {
                kind: ExprKind::Borrow(Box::new(inner)),
                span,
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_atom()?;
        while self.peek_kind(&TokenKind::Dot) {
            self.pos += 1;
            let (field, field_span) = self.expect_ident()?;
            let span = Span::new(expr.span.start.copy(), field_span.end.copy());
            expr = Expr {
                kind: ExprKind::FieldAccess(FieldAccess {
                    base: Box::new(expr),
                    field,
                    field_span,
                }),
                span,
            };
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

    fn parse_int_lit(&mut self) -> Result<Expr, Error> {
        let tok = &self.tokens[self.pos];
        match &tok.kind {
            TokenKind::IntLit(n) => {
                let value = *n;
                let span = tok.span.copy();
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::UsizeLit(value),
                    span,
                })
            }
            _ => unreachable!(),
        }
    }

    fn parse_path_atom(&mut self) -> Result<Expr, Error> {
        let path = self.parse_path()?;
        if self.peek_kind(&TokenKind::LParen) {
            let args = self.parse_call_args()?;
            let end = self.tokens[self.pos - 1].span.end.copy();
            let span = Span::new(path.span.start.copy(), end);
            Ok(Expr {
                kind: ExprKind::Call(Call {
                    callee: path,
                    args,
                }),
                span,
            })
        } else if self.peek_kind(&TokenKind::LBrace) {
            let fields = self.parse_struct_init()?;
            let end = self.tokens[self.pos - 1].span.end.copy();
            let span = Span::new(path.span.start.copy(), end);
            Ok(Expr {
                kind: ExprKind::StructLit(StructLit { path, fields }),
                span,
            })
        } else if path.segments.len() == 1 {
            let seg = &path.segments[0];
            Ok(Expr {
                kind: ExprKind::Var(seg.name.clone()),
                span: seg.span.copy(),
            })
        } else {
            Err(Error {
                file: self.file.clone(),
                message: "expected `(` or `{` after multi-segment path".to_string(),
                span: path.span.copy(),
            })
        }
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
        let (first_name, first_span) = self.expect_ident()?;
        let start = first_span.start.copy();
        let mut end = first_span.end.copy();
        let mut segments = Vec::new();
        segments.push(PathSegment {
            name: first_name,
            span: first_span,
        });
        while self.peek_kind(&TokenKind::PathSep) {
            self.pos += 1;
            let (name, span) = self.expect_ident()?;
            end = span.end.copy();
            segments.push(PathSegment { name, span });
        }
        Ok(Path {
            segments,
            span: Span::new(start, end),
        })
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
