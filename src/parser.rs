use crate::ast::{
    AssignStmt, AssocConstraint, Block, Call, Closure, ClosureParam, DeriveClause, DeriveTrait,
    EnumDef, EnumVariant, Expr, ExprKind, FieldAccess, FieldInit, FieldPattern, Function, IfExpr,
    IfLetExpr, ImplAssocType, ImplBlock, LetStmt, Lifetime, LifetimeParam, MatchArm, MatchExpr,
    MethodCall, Param, Path, PathSegment, Pattern, PatternKind, Stmt, StructDef, StructField,
    StructLit, TraitAssocType, TraitBound, TraitDef, TraitMethodSig, Type, TypeAlias, TypeKind,
    TypeParam, UseDecl, UseTree, VariantPayload,
};
use crate::lexer::{Token, TokenKind, token_kind_name};
use crate::span::{Error, Pos, Span};

pub enum RawItem {
    Function(Function),
    Struct(StructDef),
    Enum(EnumDef),
    ModDecl { name: String, name_span: Span },
    Impl(ImplBlock),
    Trait(TraitDef),
    Use(UseDecl),
    TypeAlias(TypeAlias),
}

// Attach captured `#[derive(...)]` clauses to the following item.
// Only struct and enum decls accept derives; everything else (fn, impl,
// trait, mod, use) rejects with an explicit diagnostic so misplaced
// attributes don't silently fall off.
fn attach_derives(file: &str, item: RawItem, attrs: Vec<DeriveClause>) -> Result<RawItem, Error> {
    if attrs.is_empty() {
        return Ok(item);
    }
    match item {
        RawItem::Struct(mut sd) => {
            sd.derives = attrs;
            Ok(RawItem::Struct(sd))
        }
        RawItem::Enum(mut ed) => {
            ed.derives = attrs;
            Ok(RawItem::Enum(ed))
        }
        _ => {
            let span = attrs[0].attr_span.copy();
            Err(Error {
                file: file.to_string(),
                message: "`#[derive(...)]` is only allowed on `struct` or `enum` declarations"
                    .to_string(),
                span,
            })
        }
    }
}

pub fn parse(file: &str, tokens: Vec<Token>) -> Result<Vec<RawItem>, Error> {
    let mut p = Parser {
        file: file.to_string(),
        tokens,
        pos: 0,
        next_node_id: 0,
        no_struct_lit: false,
    };
    p.parse_items()
}

struct Parser {
    file: String,
    tokens: Vec<Token>,
    pos: usize,
    // Resets at parse_function entry; captured as Function.node_count at exit.
    next_node_id: u32,
    // When true, `path { ... }` does not parse as a struct literal — it
    // stops at the path so the trailing `{` can open a then/else block.
    // Set while parsing an `if` condition; cleared inside parens. Mirrors
    // rustc's `restrictions` flag.
    no_struct_lit: bool,
}

impl Parser {
    fn parse_items(&mut self) -> Result<Vec<RawItem>, Error> {
        let mut items = Vec::new();
        while self.pos < self.tokens.len() {
            // `#[derive(...)]` only attaches to struct/enum decls — we
            // collect attributes here and stash them on the def. The
            // separate `derive_expand` stage between parser and typeck
            // walks the items list and synthesizes the corresponding
            // trait impls.
            let attrs = self.parse_attributes()?;
            let item = self.parse_item()?;
            let item = attach_derives(&self.file, item, attrs)?;
            items.push(item);
        }
        Ok(items)
    }

    fn parse_item(&mut self) -> Result<RawItem, Error> {
        // Optional `pub` prefix on item declarations. Captured here and
        // threaded into the specific AST node by each parser. `impl` and
        // `mod` don't carry their own visibility yet (impl methods are
        // pub'd individually; module visibility isn't enforced).
        let is_pub = if self.peek_kind(&TokenKind::Pub) {
            self.pos += 1;
            true
        } else {
            false
        };
        if self.peek_kind(&TokenKind::Mod) {
            // `pub mod` is accepted but currently has no extra effect —
            // module items inside still carry their own `pub`.
            self.parse_mod_decl()
        } else if self.peek_kind(&TokenKind::Fn) || self.peek_kind(&TokenKind::Unsafe) {
            // `parse_function_with_vis` handles the optional leading
            // `unsafe` token before `fn`.
            Ok(RawItem::Function(self.parse_function_with_vis(is_pub)?))
        } else if self.peek_kind(&TokenKind::Struct) {
            Ok(RawItem::Struct(self.parse_struct_def_with_vis(is_pub)?))
        } else if self.peek_kind(&TokenKind::Enum) {
            Ok(RawItem::Enum(self.parse_enum_def_with_vis(is_pub)?))
        } else if self.peek_kind(&TokenKind::Impl) {
            // `pub impl` isn't a real Rust thing; reject if seen.
            if is_pub {
                return Err(self.error_at_current("`pub` is not allowed on `impl` blocks"));
            }
            Ok(RawItem::Impl(self.parse_impl_block()?))
        } else if self.peek_kind(&TokenKind::Trait) {
            Ok(RawItem::Trait(self.parse_trait_def_with_vis(is_pub)?))
        } else if self.peek_kind(&TokenKind::Use) {
            Ok(RawItem::Use(self.parse_use_decl_with_vis(is_pub)?))
        } else if self.peek_kind(&TokenKind::Type) {
            Ok(RawItem::TypeAlias(self.parse_type_alias_with_vis(is_pub)?))
        } else {
            Err(self.error_at_current(
                "expected `fn`, `mod`, `struct`, `enum`, `impl`, `trait`, `type`, or `use`",
            ))
        }
    }

    fn parse_type_alias_with_vis(&mut self, is_pub: bool) -> Result<TypeAlias, Error> {
        let type_span = self.expect(&TokenKind::Type, "`type`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::Eq, "`=` after type alias name")?;
        let target = self.parse_type()?;
        let semi = self.expect(&TokenKind::Semi, "`;` to terminate type alias")?;
        let span = Span::new(type_span.start, semi.end);
        Ok(TypeAlias {
            name,
            name_span,
            lifetime_params,
            type_params,
            target,
            is_pub,
            span,
        })
    }

    // Parse zero-or-more `#[derive(Trait, Trait, ...)]` attributes
    // preceding the next item. Returns the captured clauses; an empty
    // result means no attributes (the common case). The only attribute
    // recognized is `derive` — anything else is rejected at parse
    // time so typos surface immediately.
    fn parse_attributes(&mut self) -> Result<Vec<DeriveClause>, Error> {
        let mut out: Vec<DeriveClause> = Vec::new();
        while self.peek_kind(&TokenKind::Hash) {
            let hash_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            self.expect(&TokenKind::LBracket, "`[` after `#`")?;
            let (name, name_span) = self.expect_ident()?;
            if name != "derive" {
                return Err(Error {
                    file: self.file.clone(),
                    message: format!(
                        "unknown attribute `{}` — only `derive` is supported",
                        name
                    ),
                    span: name_span,
                });
            }
            self.expect(&TokenKind::LParen, "`(` after `derive`")?;
            let mut traits: Vec<DeriveTrait> = Vec::new();
            if !self.peek_kind(&TokenKind::RParen) {
                let (t_name, t_span) = self.expect_ident()?;
                traits.push(DeriveTrait { name: t_name, name_span: t_span });
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RParen) {
                        break;
                    }
                    let (t_name, t_span) = self.expect_ident()?;
                    traits.push(DeriveTrait { name: t_name, name_span: t_span });
                }
            }
            self.expect(&TokenKind::RParen, "`)`")?;
            let close = self.expect(&TokenKind::RBracket, "`]`")?;
            let attr_span = Span::new(hash_span.start, close.end);
            if traits.is_empty() {
                return Err(Error {
                    file: self.file.clone(),
                    message: "`#[derive(...)]` requires at least one trait".to_string(),
                    span: attr_span,
                });
            }
            out.push(DeriveClause { traits, attr_span });
        }
        Ok(out)
    }

    // `use a::b::c;` / `use a::*;` / `use a::b as c;` / `use a::{...};`
    fn parse_use_decl(&mut self) -> Result<UseDecl, Error> {
        self.parse_use_decl_with_vis(false)
    }

    fn parse_use_decl_with_vis(&mut self, is_pub: bool) -> Result<UseDecl, Error> {
        let use_span = self.expect(&TokenKind::Use, "`use`")?;
        let tree = self.parse_use_tree()?;
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        let span = Span::new(use_span.start, semi.end);
        Ok(UseDecl { tree, is_pub, span })
    }

    // Parse one node of a use tree. Recognizes:
    //   - `*` → `Glob { path: [] }` (callers prepend a prefix).
    //   - `{ a, b, c }` → `Nested { prefix: [], children: [a, b, c] }`.
    //   - `<seg>::<seg>::...` (a path), then optionally:
    //       - `::*` → Glob with the path as prefix
    //       - `::{...}` → Nested with the path as prefix
    //       - ` as <ident>` → Leaf with rename
    //       - nothing → bare Leaf
    fn parse_use_tree(&mut self) -> Result<UseTree, Error> {
        let start_span = if self.pos < self.tokens.len() {
            self.tokens[self.pos].span.copy()
        } else {
            self.eof_span()
        };
        // Brace-only form: `{ a, b, c }` with no leading prefix.
        if self.peek_kind(&TokenKind::LBrace) {
            return self.parse_use_tree_brace_body(Vec::new(), &start_span);
        }
        // Glob with no prefix: `*` (parses but flatten will treat path as
        // empty — the caller's prefix becomes the glob target).
        if self.peek_kind(&TokenKind::Star) {
            let star = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let span = Span::new(start_span.start, star.end);
            return Ok(UseTree::Glob {
                path: Vec::new(),
                span,
            });
        }
        // `self` as a use-tree leaf — only meaningful inside a brace
        // group, where `use foo::{self, Bar};` re-imports `foo` itself
        // alongside its children. Encode as a Leaf with empty path so
        // `flatten_use_tree`'s prefix-extension produces the brace's
        // prefix as the imported absolute path. The local name comes
        // from the prefix's last segment, matching the standard
        // semantics. An optional rename (`self as foo_mod`) overrides.
        if self.peek_kind(&TokenKind::SelfLower) {
            let self_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let rename = if self.pos < self.tokens.len()
                && matches!(self.tokens[self.pos].kind, TokenKind::As)
            {
                self.pos += 1;
                let (name, _) = self.expect_ident()?;
                Some(name)
            } else {
                None
            };
            let end = self.tokens[self.pos.saturating_sub(1)].span.end.copy();
            // Sentinel "self" path; flatten_use_tree interprets the
            // leaf as the brace's prefix (the actual imported module).
            return Ok(UseTree::Leaf {
                path: vec!["self".to_string()],
                rename,
                span: Span::new(self_span.start, end),
            });
        }
        // Otherwise we have at least one path segment.
        let mut segments: Vec<String> = Vec::new();
        let (first, _) = self.expect_ident()?;
        segments.push(first);
        loop {
            if self.peek_kind(&TokenKind::PathSep) {
                self.pos += 1;
                if self.peek_kind(&TokenKind::Star) {
                    let star = self.tokens[self.pos].span.copy();
                    self.pos += 1;
                    let span = Span::new(start_span.start, star.end);
                    return Ok(UseTree::Glob {
                        path: segments,
                        span,
                    });
                }
                if self.peek_kind(&TokenKind::LBrace) {
                    return self.parse_use_tree_brace_body(segments, &start_span);
                }
                let (next, _) = self.expect_ident()?;
                segments.push(next);
                continue;
            }
            break;
        }
        // Optional rename: `as ident`.
        let rename = if self.pos < self.tokens.len() {
            // We need to detect `as`. There's a TokenKind::As.
            if matches!(self.tokens[self.pos].kind, TokenKind::As) {
                self.pos += 1;
                let (name, _) = self.expect_ident()?;
                Some(name)
            } else {
                None
            }
        } else {
            None
        };
        let end = self.tokens[self.pos.saturating_sub(1)].span.end.copy();
        let span = Span::new(start_span.start, end);
        Ok(UseTree::Leaf {
            path: segments,
            rename,
            span,
        })
    }

    fn parse_use_tree_brace_body(
        &mut self,
        prefix: Vec<String>,
        start_span: &Span,
    ) -> Result<UseTree, Error> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut children: Vec<UseTree> = Vec::new();
        if !self.peek_kind(&TokenKind::RBrace) {
            children.push(self.parse_use_tree()?);
            while self.peek_kind(&TokenKind::Comma) {
                self.pos += 1;
                if self.peek_kind(&TokenKind::RBrace) {
                    break;
                }
                children.push(self.parse_use_tree()?);
            }
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        let span = Span::new(start_span.start.copy(), rb.end);
        Ok(UseTree::Nested {
            prefix,
            children,
            span,
        })
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
        let mut assoc_type_bindings: Vec<ImplAssocType> = Vec::new();
        while !self.peek_kind(&TokenKind::RBrace) {
            if self.peek_kind(&TokenKind::Type) {
                self.pos += 1;
                let (aname, aname_span) = self.expect_ident()?;
                self.expect(&TokenKind::Eq, "`=`")?;
                let aty = self.parse_type()?;
                self.expect(&TokenKind::Semi, "`;`")?;
                assoc_type_bindings.push(ImplAssocType {
                    name: aname,
                    name_span: aname_span,
                    ty: aty,
                });
                continue;
            }
            // Optional `pub` on inherent impl methods. For trait
            // impls, methods inherit the trait's visibility — `pub`
            // is silently allowed but doesn't change anything beyond
            // the method's `is_pub` flag (which won't be checked for
            // trait-impl methods).
            let method_is_pub = if self.peek_kind(&TokenKind::Pub) {
                self.pos += 1;
                true
            } else {
                false
            };
            // `parse_function_with_vis` consumes an optional leading
            // `unsafe` token before `fn`; we just need to look past
            // that here when checking for the `fn` keyword.
            let next = if self.peek_kind(&TokenKind::Unsafe) {
                self.pos + 1
            } else {
                self.pos
            };
            if next >= self.tokens.len() || !matches!(self.tokens[next].kind, TokenKind::Fn) {
                return Err(self.error_at_current(
                    "expected `fn` or `type` inside `impl` block",
                ));
            }
            methods.push(self.parse_function_with_vis(method_is_pub)?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(ImplBlock {
            lifetime_params,
            type_params,
            trait_path,
            target,
            methods,
            assoc_type_bindings,
            span: Span::new(impl_span.start, rb.end),
        })
    }

    fn parse_trait_def(&mut self) -> Result<TraitDef, Error> {
        self.parse_trait_def_with_vis(false)
    }

    fn parse_trait_def_with_vis(&mut self, is_pub: bool) -> Result<TraitDef, Error> {
        let trait_kw = self.expect(&TokenKind::Trait, "`trait`")?;
        let (name, name_span) = self.expect_ident()?;
        // Optional `<T1, T2, ...>` trait-level type parameters.
        // Lifetime params on traits aren't supported yet.
        let type_params = if self.peek_kind(&TokenKind::LAngle) {
            let (_lifetime_params, type_params) = self.parse_generic_params()?;
            type_params
        } else {
            Vec::new()
        };
        let mut supertraits: Vec<TraitBound> = Vec::new();
        if self.peek_kind(&TokenKind::Colon) {
            self.pos += 1;
            supertraits.push(self.parse_trait_bound()?);
            while self.peek_kind(&TokenKind::Plus) {
                self.pos += 1;
                supertraits.push(self.parse_trait_bound()?);
            }
        }
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut methods: Vec<TraitMethodSig> = Vec::new();
        let mut assoc_types: Vec<TraitAssocType> = Vec::new();
        while !self.peek_kind(&TokenKind::RBrace) {
            if self.peek_kind(&TokenKind::Type) {
                self.pos += 1;
                let (aname, aname_span) = self.expect_ident()?;
                self.expect(&TokenKind::Semi, "`;`")?;
                assoc_types.push(TraitAssocType {
                    name: aname,
                    name_span: aname_span,
                });
                continue;
            }
            if !self.peek_kind(&TokenKind::Fn) {
                return Err(self.error_at_current(
                    "expected `fn` or `type` inside `trait` body",
                ));
            }
            methods.push(self.parse_trait_method_sig()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(TraitDef {
            name,
            name_span,
            type_params,
            supertraits,
            methods,
            assoc_types,
            span: Span::new(trait_kw.start, rb.end),
            is_pub,
        })
    }

    // Trait method signature: same shape as `parse_function` but ends in `;`
    // (no body). Receiver shorthand allowed (`&self`, `&mut self`, `self`).
    fn parse_trait_method_sig(&mut self) -> Result<TraitMethodSig, Error> {
        self.expect(&TokenKind::Fn, "`fn`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, mut type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = if self.peek_kind(&TokenKind::RParen) {
            Vec::new()
        } else {
            self.parse_params()?
        };
        self.expect(&TokenKind::RParen, "`)`")?;
        self.desugar_apit(&mut params, &mut type_params);
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
        self.parse_struct_def_with_vis(false)
    }

    fn parse_struct_def_with_vis(&mut self, is_pub: bool) -> Result<StructDef, Error> {
        self.expect(&TokenKind::Struct, "`struct`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        // Unit struct: `struct Foo;` / `struct Foo<T>;` — no body, terminated
        // with `;`. Construction syntax is the empty struct-lit form `Foo {}`
        // (no bare-ident form to avoid clashing with variable references).
        let mut fields = Vec::new();
        if self.peek_kind(&TokenKind::Semi) {
            self.pos += 1;
            return Ok(StructDef {
                name,
                name_span,
                lifetime_params,
                type_params,
                fields,
                is_pub,
                derives: Vec::new(),
            });
        }
        self.expect(&TokenKind::LBrace, "`{` or `;`")?;
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
            is_pub,
            derives: Vec::new(),
        })
    }

    fn parse_struct_field(&mut self) -> Result<StructField, Error> {
        let is_pub = if self.peek_kind(&TokenKind::Pub) {
            self.pos += 1;
            true
        } else {
            false
        };
        let (name, name_span) = self.expect_ident()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        Ok(StructField {
            name,
            name_span,
            ty,
            is_pub,
        })
    }

    fn parse_enum_def_with_vis(&mut self, is_pub: bool) -> Result<EnumDef, Error> {
        self.expect(&TokenKind::Enum, "`enum`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut variants: Vec<EnumVariant> = Vec::new();
        if !self.peek_kind(&TokenKind::RBrace) {
            variants.push(self.parse_enum_variant()?);
            while self.peek_kind(&TokenKind::Comma) {
                self.pos += 1;
                if self.peek_kind(&TokenKind::RBrace) {
                    break;
                }
                variants.push(self.parse_enum_variant()?);
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(EnumDef {
            name,
            name_span,
            lifetime_params,
            type_params,
            variants,
            is_pub,
            derives: Vec::new(),
        })
    }

    fn parse_enum_variant(&mut self) -> Result<EnumVariant, Error> {
        let (name, name_span) = self.expect_ident()?;
        let payload = if self.peek_kind(&TokenKind::LParen) {
            // Tuple variant: `A(T1, T2, …)`. Empty `A()` is allowed
            // (parses as a 0-element tuple variant — same shape as
            // unit, but distinguishes the surface form).
            self.pos += 1;
            let mut elems: Vec<Type> = Vec::new();
            if !self.peek_kind(&TokenKind::RParen) {
                elems.push(self.parse_type()?);
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RParen) {
                        break;
                    }
                    elems.push(self.parse_type()?);
                }
            }
            self.expect(&TokenKind::RParen, "`)`")?;
            VariantPayload::Tuple(elems)
        } else if self.peek_kind(&TokenKind::LBrace) {
            // Struct variant: `A { f: T, g: U }`.
            self.pos += 1;
            let mut fields: Vec<StructField> = Vec::new();
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
            VariantPayload::Struct(fields)
        } else {
            VariantPayload::Unit
        };
        Ok(EnumVariant {
            name,
            name_span,
            payload,
        })
    }

    // Desugar argument-position `impl Trait` into anonymous type
    // parameters. Walks each param.ty: when it's `TypeKind::ImplTrait`
    // at the top level, allocates a fresh `__impl_<N>` name (counter
    // local to this fn signature, plus any pre-existing `__impl_*` to
    // keep the names disjoint), appends a `TypeParam` carrying the
    // bounds, and replaces the param's type with a Path to that name.
    // Nested `impl Trait` (e.g. `Vec<impl Trait>`) is left alone — it
    // will surface a "not allowed here" error at typeck.
    fn desugar_apit(
        &mut self,
        params: &mut Vec<Param>,
        type_params: &mut Vec<TypeParam>,
    ) {
        let mut counter: u32 = 0;
        let mut i = 0;
        while i < params.len() {
            let take = matches!(&params[i].ty.kind, TypeKind::ImplTrait(_));
            if take {
                let span = params[i].ty.span.copy();
                let bounds = match std::mem::replace(
                    &mut params[i].ty.kind,
                    TypeKind::Tuple(Vec::new()),
                ) {
                    TypeKind::ImplTrait(b) => b,
                    _ => unreachable!("just checked"),
                };
                // Pick a name that doesn't collide with an existing
                // user type-param (rare, but cheap to defend against).
                let name = loop {
                    let candidate = format!("__impl_{}", counter);
                    counter += 1;
                    let mut clash = false;
                    let mut k = 0;
                    while k < type_params.len() {
                        if type_params[k].name == candidate {
                            clash = true;
                            break;
                        }
                        k += 1;
                    }
                    if !clash {
                        break candidate;
                    }
                };
                let path = Path {
                    segments: vec![PathSegment {
                        name: name.clone(),
                        span: span.copy(),
                        lifetime_args: Vec::new(),
                        args: Vec::new(),
                    }],
                    span: span.copy(),
                };
                params[i].ty = Type {
                    kind: TypeKind::Path(path),
                    span: span.copy(),
                };
                type_params.push(TypeParam {
                    name,
                    name_span: span,
                    bounds,
                    default: None,
                });
            }
            i += 1;
        }
    }

    fn parse_function(&mut self) -> Result<Function, Error> {
        self.parse_function_with_vis(false)
    }

    fn parse_function_with_vis(&mut self, is_pub: bool) -> Result<Function, Error> {
        // Optional `unsafe` modifier before `fn`. `pub unsafe fn` /
        // `unsafe fn` both work; `unsafe pub fn` is rejected (Rust's
        // canonical order is `pub unsafe fn`).
        let is_unsafe = if self.peek_kind(&TokenKind::Unsafe) {
            self.pos += 1;
            true
        } else {
            false
        };
        self.expect(&TokenKind::Fn, "`fn`")?;
        let (name, name_span) = self.expect_ident()?;
        let (lifetime_params, mut type_params) = if self.peek_kind(&TokenKind::LAngle) {
            self.parse_generic_params()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = if self.peek_kind(&TokenKind::RParen) {
            Vec::new()
        } else {
            self.parse_params()?
        };
        self.expect(&TokenKind::RParen, "`)`")?;
        // Desugar argument-position `impl Trait` after params and any
        // user-written `<…>` type-params are both known, so the synth
        // names can avoid colliding.
        self.desugar_apit(&mut params, &mut type_params);
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
            is_pub,
            is_unsafe,
        })
    }

    fn alloc_node_id(&mut self) -> crate::ast::NodeId {
        let id = self.next_node_id;
        self.next_node_id += 1;
        id
    }

    // Top-level pattern entry. Or-patterns (`p | q | r`) have lowest
    // precedence — left-associative chain at the top.
    fn parse_pattern(&mut self) -> Result<Pattern, Error> {
        let first = self.parse_pattern_no_or()?;
        if !self.peek_kind(&TokenKind::Pipe) {
            return Ok(first);
        }
        let start = first.span.start.copy();
        let mut alternatives: Vec<Pattern> = Vec::new();
        alternatives.push(first);
        while self.peek_kind(&TokenKind::Pipe) {
            self.pos += 1;
            alternatives.push(self.parse_pattern_no_or()?);
        }
        let end = alternatives[alternatives.len() - 1].span.end.copy();
        let span = Span::new(start, end);
        let id = self.alloc_node_id();
        Ok(Pattern {
            kind: PatternKind::Or(alternatives),
            span,
            id,
        })
    }

    // Single non-or pattern. Handles `_`, literals, `&pat`/`&mut pat`,
    // tuple patterns, paths (which become Ident, VariantTuple, or
    // VariantStruct), and at-bindings `name @ subpat`. Range patterns
    // use the literal start as the left endpoint.
    fn parse_pattern_no_or(&mut self) -> Result<Pattern, Error> {
        if self.pos >= self.tokens.len() {
            return Err(self.error_at_current("expected pattern, got end of input"));
        }
        // `_` — wildcard.
        if self.peek_kind(&TokenKind::Underscore) {
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let id = self.alloc_node_id();
            return Ok(Pattern { kind: PatternKind::Wildcard, span, id });
        }
        // `&pat` / `&mut pat`.
        if self.peek_kind(&TokenKind::Amp) {
            let amp_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let mutable = if self.peek_kind(&TokenKind::Mut) {
                self.pos += 1;
                true
            } else {
                false
            };
            let inner = self.parse_pattern_no_or()?;
            let span = Span::new(amp_span.start, inner.span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Pattern {
                kind: PatternKind::Ref { inner: Box::new(inner), mutable },
                span,
                id,
            });
        }
        // `(p, q, ...)` — tuple pattern. `()` is the unit pattern;
        // `(p,)` is 1-element. Single `(p)` falls through as `p`.
        if self.peek_kind(&TokenKind::LParen) {
            let lp = self.tokens[self.pos].span.copy();
            self.pos += 1;
            if self.peek_kind(&TokenKind::RParen) {
                let rp = self.expect(&TokenKind::RParen, "`)`")?;
                let id = self.alloc_node_id();
                return Ok(Pattern {
                    kind: PatternKind::Tuple(Vec::new()),
                    span: Span::new(lp.start, rp.end),
                    id,
                });
            }
            let first = self.parse_pattern()?;
            if self.peek_kind(&TokenKind::Comma) {
                let mut elems: Vec<Pattern> = Vec::new();
                elems.push(first);
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RParen) {
                        break;
                    }
                    elems.push(self.parse_pattern()?);
                }
                let rp = self.expect(&TokenKind::RParen, "`)`")?;
                let id = self.alloc_node_id();
                return Ok(Pattern {
                    kind: PatternKind::Tuple(elems),
                    span: Span::new(lp.start, rp.end),
                    id,
                });
            }
            self.expect(&TokenKind::RParen, "`)`")?;
            return Ok(first);
        }
        // Integer literal — possibly the lower end of a range pattern
        // (`lo..=hi`).
        if let TokenKind::IntLit(n) = self.tokens[self.pos].kind {
            let lit_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            if self.peek_kind(&TokenKind::DotDotEq) {
                self.pos += 1;
                if let TokenKind::IntLit(hi) = self.tokens[self.pos].kind {
                    let end_span = self.tokens[self.pos].span.copy();
                    self.pos += 1;
                    let span = Span::new(lit_span.start, end_span.end);
                    let id = self.alloc_node_id();
                    return Ok(Pattern {
                        kind: PatternKind::Range { lo: n, hi },
                        span,
                        id,
                    });
                }
                return Err(self.error_at_current("expected integer literal after `..=`"));
            }
            let id = self.alloc_node_id();
            return Ok(Pattern {
                kind: PatternKind::LitInt(n),
                span: lit_span,
                id,
            });
        }
        // Boolean literal.
        if self.peek_kind(&TokenKind::True) {
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let id = self.alloc_node_id();
            return Ok(Pattern { kind: PatternKind::LitBool(true), span, id });
        }
        if self.peek_kind(&TokenKind::False) {
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let id = self.alloc_node_id();
            return Ok(Pattern { kind: PatternKind::LitBool(false), span, id });
        }
        // `ref name` / `ref mut name` — binds a reference to the matched
        // place rather than moving/copying the value. `mut name` (no
        // ref) binds by-value with the binding marked mutable so the
        // arm body can assign through it.
        if self.peek_kind(&TokenKind::Ref) {
            let ref_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let mutable = if self.peek_kind(&TokenKind::Mut) {
                self.pos += 1;
                true
            } else {
                false
            };
            let (name, name_span) = self.expect_ident()?;
            let span = Span::new(ref_span.start, name_span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Pattern {
                kind: PatternKind::Binding { name, name_span, by_ref: true, mutable },
                span,
                id,
            });
        }
        if self.peek_kind(&TokenKind::Mut) {
            let mut_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let (name, name_span) = self.expect_ident()?;
            let span = Span::new(mut_span.start, name_span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Pattern {
                kind: PatternKind::Binding { name, name_span, by_ref: false, mutable: true },
                span,
                id,
            });
        }
        // Path-prefixed pattern: `Ident` (binding/variant), `Ident @ pat`,
        // `Ident::…(args)` (variant tuple), `Ident::… { fields }` (variant
        // struct), or just `Ident::…` (path-only — unit variant).
        if matches!(&self.tokens[self.pos].kind, TokenKind::Ident(_)) {
            let first_span = self.tokens[self.pos].span.copy();
            // Check whether it's a single ident followed by `@` or by
            // anything that's NOT a path-continuation. Single-ident is
            // ambiguous: `x` could be a binding *or* a unit-variant
            // reference (e.g. `Some` in scope). We always parse it as
            // an `Ident` pattern; typeck distinguishes against the
            // active enum table.
            let is_simple_ident = !self.peek_two(&TokenKind::Ident("".to_string()), &TokenKind::PathSep)
                && !matches!(
                    self.tokens.get(self.pos + 1).map(|t| &t.kind),
                    Some(TokenKind::PathSep) | Some(TokenKind::LParen) | Some(TokenKind::LBrace)
                );
            if is_simple_ident {
                let (name, name_span) = self.expect_ident()?;
                if self.peek_kind(&TokenKind::At) {
                    self.pos += 1;
                    let inner = self.parse_pattern_no_or()?;
                    let span = Span::new(first_span.start, inner.span.end.copy());
                    let id = self.alloc_node_id();
                    return Ok(Pattern {
                        kind: PatternKind::At { name, name_span, inner: Box::new(inner) },
                        span,
                        id,
                    });
                }
                let id = self.alloc_node_id();
                return Ok(Pattern {
                    kind: PatternKind::Binding {
                        name,
                        name_span: name_span.copy(),
                        by_ref: false,
                        mutable: false,
                    },
                    span: name_span,
                    id,
                });
            }
            // Multi-segment path or single ident followed by `(`/`{`.
            let path = self.parse_path_with_type_args()?;
            let path_span = path.span.copy();
            if self.peek_kind(&TokenKind::LParen) {
                self.pos += 1;
                let mut elems: Vec<Pattern> = Vec::new();
                if !self.peek_kind(&TokenKind::RParen) {
                    elems.push(self.parse_pattern()?);
                    while self.peek_kind(&TokenKind::Comma) {
                        self.pos += 1;
                        if self.peek_kind(&TokenKind::RParen) {
                            break;
                        }
                        elems.push(self.parse_pattern()?);
                    }
                }
                let rp = self.expect(&TokenKind::RParen, "`)`")?;
                let span = Span::new(path_span.start, rp.end);
                let id = self.alloc_node_id();
                return Ok(Pattern {
                    kind: PatternKind::VariantTuple { path, elems },
                    span,
                    id,
                });
            }
            if self.peek_kind(&TokenKind::LBrace) {
                self.pos += 1;
                let mut fields: Vec<FieldPattern> = Vec::new();
                let mut rest = false;
                if !self.peek_kind(&TokenKind::RBrace) {
                    loop {
                        if self.peek_kind(&TokenKind::DotDot) {
                            self.pos += 1;
                            rest = true;
                            break;
                        }
                        let (name, name_span) = self.expect_ident()?;
                        let pattern = if self.peek_kind(&TokenKind::Colon) {
                            self.pos += 1;
                            self.parse_pattern()?
                        } else {
                            // Shorthand: `Foo { a }` ≡ `Foo { a: a }`,
                            // where the inner pattern is a binding for `a`.
                            let id = self.alloc_node_id();
                            Pattern {
                                kind: PatternKind::Binding {
                                    name: name.clone(),
                                    name_span: name_span.copy(),
                                    by_ref: false,
                                    mutable: false,
                                },
                                span: name_span.copy(),
                                id,
                            }
                        };
                        fields.push(FieldPattern { name, name_span, pattern });
                        if self.peek_kind(&TokenKind::Comma) {
                            self.pos += 1;
                            if self.peek_kind(&TokenKind::RBrace) {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                let rb = self.expect(&TokenKind::RBrace, "`}`")?;
                let span = Span::new(path_span.start, rb.end);
                let id = self.alloc_node_id();
                return Ok(Pattern {
                    kind: PatternKind::VariantStruct { path, fields, rest },
                    span,
                    id,
                });
            }
            // Bare path — treat as a unit-variant reference. (No
            // arguments, no struct-style braces.) Routed through
            // VariantTuple with an empty `elems` list so typeck can
            // handle "construct a variant" uniformly.
            let id = self.alloc_node_id();
            return Ok(Pattern {
                kind: PatternKind::VariantTuple { path, elems: Vec::new() },
                span: path_span,
                id,
            });
        }
        Err(self.error_at_current("expected pattern"))
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
        let default = if self.peek_kind(&TokenKind::Eq) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        Ok(TypeParam { name, name_span, bounds, default })
    }

    fn parse_trait_bound(&mut self) -> Result<TraitBound, Error> {
        // A bound is `Path<Args, Name = Type, …>`. Args (positional
        // type-args for the trait's `<T1, T2, …>` declaration) come
        // first; assoc-type constraints (`Name = Type`) follow. Each
        // entry is disambiguated by peeking: an `IDENT =` pair is an
        // assoc constraint; anything else is a type-arg expression.
        //
        // Optional HRTB prefix: `for<'a, 'b> Path<…>`. Lifetimes here
        // scope only into the bound's own path / args / assoc-constraint
        // types — they don't leak to the enclosing fn/impl scope. Used
        // for closure bounds like `for<'a> Fn(&'a T) -> R`.
        //
        // Parenthesized sugar: `Path(T1, T2) -> R` rewrites to
        // `Path<(T1, T2), Output = R>`. Used by Fn/FnMut/FnOnce. The
        // tuple is built from the parens' contents; an absent `-> R`
        // defaults to `()` (matching Rust). `<…>` and `(…)` are
        // mutually exclusive — `Fn<(T,)>(U)` is a parse error.
        let mut hrtb_lifetime_params: Vec<LifetimeParam> = Vec::new();
        if self.peek_kind(&TokenKind::For) {
            self.pos += 1;
            self.expect(&TokenKind::LAngle, "`<`")?;
            if !self.peek_kind(&TokenKind::RAngle) {
                let (name, name_span) = self.expect_lifetime()?;
                hrtb_lifetime_params.push(LifetimeParam { name, name_span });
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RAngle) {
                        break;
                    }
                    let (name, name_span) = self.expect_lifetime()?;
                    hrtb_lifetime_params.push(LifetimeParam { name, name_span });
                }
            }
            self.expect(&TokenKind::RAngle, "`>`")?;
        }
        let mut path = self.parse_path()?;
        let mut trait_type_args: Vec<Type> = Vec::new();
        let mut assoc_constraints: Vec<AssocConstraint> = Vec::new();
        if self.peek_kind(&TokenKind::LParen) {
            let lparen_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let mut elem_tys: Vec<Type> = Vec::new();
            if !self.peek_kind(&TokenKind::RParen) {
                elem_tys.push(self.parse_type()?);
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::RParen) {
                        break;
                    }
                    elem_tys.push(self.parse_type()?);
                }
            }
            let rparen_span = self.expect(&TokenKind::RParen, "`)`")?;
            let tuple_span = Span::new(lparen_span.start.copy(), rparen_span.end.copy());
            let tuple_ty = Type {
                kind: TypeKind::Tuple(elem_tys),
                span: tuple_span,
            };
            trait_type_args.push(tuple_ty);
            let (output_ty, output_span) = if self.peek_kind(&TokenKind::Arrow) {
                let arrow_span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let ty = self.parse_type()?;
                let span = Span::new(arrow_span.start.copy(), ty.span.end.copy());
                (ty, span)
            } else {
                let unit_span = rparen_span.copy();
                (
                    Type {
                        kind: TypeKind::Tuple(Vec::new()),
                        span: unit_span.copy(),
                    },
                    unit_span,
                )
            };
            assoc_constraints.push(AssocConstraint {
                name: "Output".to_string(),
                name_span: output_span,
                ty: output_ty,
            });
            // Parenthesized sugar precludes `<…>`.
            if !trait_type_args.is_empty() {
                let last = path.segments.len() - 1;
                path.segments[last].args = trait_type_args;
            }
            return Ok(TraitBound { path, assoc_constraints, hrtb_lifetime_params });
        }
        if self.peek_kind(&TokenKind::LAngle) {
            self.pos += 1;
            while !self.peek_kind(&TokenKind::RAngle) {
                let is_assoc = matches!(
                    (
                        self.tokens.get(self.pos).map(|t| &t.kind),
                        self.tokens.get(self.pos + 1).map(|t| &t.kind),
                    ),
                    (Some(TokenKind::Ident(_)), Some(TokenKind::Eq))
                );
                if is_assoc {
                    let (cname, cname_span) = self.expect_ident()?;
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let cty = self.parse_type()?;
                    assoc_constraints.push(AssocConstraint {
                        name: cname,
                        name_span: cname_span,
                        ty: cty,
                    });
                } else {
                    let ty = self.parse_type()?;
                    trait_type_args.push(ty);
                }
                if self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::RAngle, "`>`")?;
        }
        // Stash the positional type-args on the path's last segment
        // so downstream resolution sees them like any other typed
        // path. (This mirrors how struct paths carry their `<…>`.)
        if !trait_type_args.is_empty() {
            let last = path.segments.len() - 1;
            path.segments[last].args = trait_type_args;
        }
        Ok(TraitBound { path, assoc_constraints, hrtb_lifetime_params })
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
        if self.peek_kind(&TokenKind::LParen) {
            // Tuple types: `()`, `(T,)`, `(T1, T2, ...)`. The 1-tuple
            // form requires a trailing comma to disambiguate from
            // a parenthesized `(T)`.
            return self.parse_tuple_type();
        }
        if self.peek_kind(&TokenKind::Impl) {
            // `impl T1 + T2 + …` — argument-position impl trait. Only
            // valid at the top of a fn parameter type; the function
            // parser desugars matching params to anonymous type-params
            // after the params list is parsed. Anywhere else, this
            // ImplTrait survives into typeck and is rejected.
            let impl_span = self.expect(&TokenKind::Impl, "`impl`")?;
            let mut bounds: Vec<TraitBound> = Vec::new();
            bounds.push(self.parse_trait_bound()?);
            while self.peek_kind(&TokenKind::Plus) {
                self.pos += 1;
                bounds.push(self.parse_trait_bound()?);
            }
            let end = self.tokens[self.pos.saturating_sub(1)].span.end.copy();
            let span = Span::new(impl_span.start, end);
            return Ok(Type {
                kind: TypeKind::ImplTrait(bounds),
                span,
            });
        }
        if self.peek_kind(&TokenKind::Bang) {
            // `!` — the never type. Bare bang in type position only.
            let span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            return Ok(Type {
                kind: TypeKind::Never,
                span,
            });
        }
        if self.peek_kind(&TokenKind::SelfUpper) {
            // Bare `Self` → SelfType. `Self::Name` → fall through to
            // path parsing (`parse_path_with_type_args` handles the
            // `Self` segment via `expect_path_segment`); resolution
            // of the assoc-type projection happens at typeck time.
            if !self.peek_two(&TokenKind::SelfUpper, &TokenKind::PathSep) {
                let span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                return Ok(Type {
                    kind: TypeKind::SelfType,
                    span,
                });
            }
        }
        // In type position, `&&T` means `& &T` (a ref to a ref), not
        // logical-AND. Split the `&&` token into two `&`s so the
        // existing single-`&` branch below handles the inner ref.
        if self.peek_kind(&TokenKind::AndAnd) {
            let span = self.tokens[self.pos].span.copy();
            let mid = Pos::new(span.start.line, span.start.col + 1);
            let first = Token {
                kind: TokenKind::Amp,
                span: Span::new(span.start.copy(), mid.copy()),
            };
            let second = Token {
                kind: TokenKind::Amp,
                span: Span::new(mid, span.end.copy()),
            };
            self.tokens[self.pos] = first;
            self.tokens.insert(self.pos + 1, second);
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
        if self.peek_kind(&TokenKind::LBracket) {
            // Slice type `[T]`. Bare slices are DSTs and only valid
            // behind a reference; the type-resolver enforces that.
            let lb = self.expect(&TokenKind::LBracket, "`[`")?;
            let inner = self.parse_type()?;
            let rb = self.expect(&TokenKind::RBracket, "`]`")?;
            return Ok(Type {
                kind: TypeKind::Slice(Box::new(inner)),
                span: Span::new(lb.start, rb.end),
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

    // `()` (unit), `(T,)` (1-tuple, comma required), `(T1, T2)` (≥2-tuple,
    // trailing comma optional). The 1-tuple comma is what disambiguates
    // from a grouping `(T)`; if we see `(T)` here we still produce a
    // `TupleType` of length 1 only when the trailing comma is present —
    // a bare `(T)` falls through to a simple `T`.
    fn parse_tuple_type(&mut self) -> Result<Type, Error> {
        let lp = self.expect(&TokenKind::LParen, "`(`")?;
        if self.peek_kind(&TokenKind::RParen) {
            let rp = self.expect(&TokenKind::RParen, "`)`")?;
            return Ok(Type {
                kind: TypeKind::Tuple(Vec::new()),
                span: Span::new(lp.start, rp.end),
            });
        }
        let mut elems: Vec<Type> = Vec::new();
        elems.push(self.parse_type()?);
        let mut had_trailing_comma = false;
        while self.peek_kind(&TokenKind::Comma) {
            self.pos += 1;
            had_trailing_comma = true;
            if self.peek_kind(&TokenKind::RParen) {
                break;
            }
            had_trailing_comma = false;
            elems.push(self.parse_type()?);
        }
        let rp = self.expect(&TokenKind::RParen, "`)`")?;
        if elems.len() == 1 && !had_trailing_comma {
            // `(T)` is a parenthesized type — return T directly.
            return Ok(elems.pop().unwrap());
        }
        Ok(Type {
            kind: TypeKind::Tuple(elems),
            span: Span::new(lp.start, rp.end),
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
            if self.peek_kind(&TokenKind::Use) {
                let decl = self.parse_use_decl()?;
                stmts.push(Stmt::Use(decl));
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
            // Compound-assignment desugar: `a OP= b;` → `Stmt::Expr(
            // MethodCall { receiver: a, method: "<op>_assign", args:
            // [b] })`. Method dispatch autorefs the receiver to
            // `&mut a` (the trait method takes `&mut self`), so `a`
            // must be a mutable place — exactly the same constraint
            // as `a = a OP b;` would have.
            let op_assign_method: Option<&str> = if self.peek_kind(&TokenKind::PlusEq) {
                Some("add_assign")
            } else if self.peek_kind(&TokenKind::MinusEq) {
                Some("sub_assign")
            } else if self.peek_kind(&TokenKind::StarEq) {
                Some("mul_assign")
            } else if self.peek_kind(&TokenKind::SlashEq) {
                Some("div_assign")
            } else if self.peek_kind(&TokenKind::PercentEq) {
                Some("rem_assign")
            } else {
                None
            };
            if let Some(method) = op_assign_method {
                let op_span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let rhs = self.parse_expr()?;
                let semi = self.expect(&TokenKind::Semi, "`;`")?;
                let span = Span::new(expr.span.start.copy(), semi.end);
                let id = self.alloc_node_id();
                let call = Expr {
                    kind: ExprKind::MethodCall(MethodCall {
                        receiver: Box::new(expr),
                        method: method.to_string(),
                        method_span: op_span,
                        turbofish_args: Vec::new(),
                        args: vec![rhs],
                    }),
                    span,
                    id,
                };
                stmts.push(Stmt::Expr(call));
                continue;
            }
            if self.peek_kind(&TokenKind::Semi) {
                // Plain expression statement: `expr;` — value (if any)
                // is discarded; the expression is walked for its side
                // effects.
                self.pos += 1;
                stmts.push(Stmt::Expr(expr));
                continue;
            }
            // Block-like expressions (`unsafe { … }`, `{ … }`, `if`)
            // without a `;` can still sit as bare statements — their
            // braces already delimit them. Treat those as statements
            // when they're not the final tail-position expression.
            if is_unit_block_like(&expr) && !self.peek_kind(&TokenKind::RBrace) {
                stmts.push(Stmt::Expr(expr));
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
        let pattern = self.parse_pattern()?;
        let ty = if self.peek_kind(&TokenKind::Colon) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        // `let PAT: TYPE;` — declared but uninitialized. The `=` is
        // optional; when absent, no initializer expression follows
        // and (per typeck) the type annotation is required and the
        // pattern must be a single `Binding`.
        let (value, else_block) = if self.peek_kind(&TokenKind::Eq) {
            self.pos += 1;
            let v = self.parse_expr()?;
            // `let PAT = EXPR else { … };` (let-else). The else
            // block must diverge — enforced at typeck via the
            // block's type unifying with `!`.
            let eb = if self.peek_kind(&TokenKind::Else) {
                self.pos += 1;
                let blk = self.parse_block()?;
                Some(Box::new(blk))
            } else {
                None
            };
            (Some(v), eb)
        } else {
            (None, None)
        };
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(Stmt::Let(LetStmt {
            pattern,
            ty,
            value,
            else_block,
        }))
    }

    fn parse_expr(&mut self) -> Result<Expr, Error> {
        self.parse_range()
    }

    // Range literals — `a..b`, `a..`, `..b`, `..`, `a..=b`, `..=b`.
    // Lowest expression precedence (just above the assignment-stmt
    // level, which isn't an expression in pocket-rust). Non-associative
    // — `a..b..c` is a parse error from the inner `parse_logical_or`
    // not seeing a second `..`.
    //
    // Desugars at parse-time to struct literals of the corresponding
    // `std::ops::Range*` types so the rest of the pipeline (typeck,
    // codegen) sees ordinary struct construction. The bare path
    // (`Range`, `RangeFrom`, …) relies on the implicit `use std::*;`
    // prelude.
    fn parse_range(&mut self) -> Result<Expr, Error> {
        // Prefix range: `..end`, `..=end`, or bare `..` (RangeFull).
        if self.peek_kind(&TokenKind::DotDot) || self.peek_kind(&TokenKind::DotDotEq) {
            let inclusive = self.peek_kind(&TokenKind::DotDotEq);
            let dot_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            if self.is_range_terminator() {
                if inclusive {
                    return Err(Error {
                        file: self.file.clone(),
                        message: "`..=` requires a right-side expression".to_string(),
                        span: dot_span,
                    });
                }
                return Ok(self.build_range_full(dot_span));
            }
            let end = self.parse_logical_or()?;
            if inclusive {
                return Ok(self.build_range_to_inclusive(end, dot_span));
            }
            return Ok(self.build_range_to(end, dot_span));
        }
        let left = self.parse_logical_or()?;
        if self.peek_kind(&TokenKind::DotDot) || self.peek_kind(&TokenKind::DotDotEq) {
            let inclusive = self.peek_kind(&TokenKind::DotDotEq);
            let dot_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            if self.is_range_terminator() {
                if inclusive {
                    return Err(Error {
                        file: self.file.clone(),
                        message: "`..=` requires a right-side expression".to_string(),
                        span: dot_span,
                    });
                }
                return Ok(self.build_range_from(left, dot_span));
            }
            let right = self.parse_logical_or()?;
            if inclusive {
                return Ok(self.build_range_inclusive(left, right, dot_span));
            }
            return Ok(self.build_range(left, right, dot_span));
        }
        Ok(left)
    }

    // True iff the next token can't start an expression — used to
    // distinguish `..end` / `start..end` (right side present) from
    // `..` / `start..` (no right side).
    fn is_range_terminator(&self) -> bool {
        if self.pos >= self.tokens.len() {
            return true;
        }
        matches!(
            &self.tokens[self.pos].kind,
            TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Comma
                | TokenKind::Semi
        )
    }

    // Build a one-segment path `Range` (relying on `use std::*;`
    // prelude). Used by all range desugarings.
    fn make_range_path(&self, name: &str, span: &Span) -> Path {
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

    fn make_range_struct_lit(&mut self, name: &str, fields: Vec<(&str, Expr)>, span: Span) -> Expr {
        let path = self.make_range_path(name, &span);
        let field_inits: Vec<FieldInit> = fields
            .into_iter()
            .map(|(n, v)| FieldInit {
                name: n.to_string(),
                name_span: span.copy(),
                value: v,
            })
            .collect();
        let id = self.alloc_node_id();
        Expr {
            kind: ExprKind::StructLit(StructLit { path, fields: field_inits }),
            span,
            id,
        }
    }

    fn build_range(&mut self, start: Expr, end: Expr, _dot_span: Span) -> Expr {
        let span = Span::new(start.span.start.copy(), end.span.end.copy());
        self.make_range_struct_lit("Range", vec![("start", start), ("end", end)], span)
    }

    fn build_range_from(&mut self, start: Expr, dot_span: Span) -> Expr {
        let span = Span::new(start.span.start.copy(), dot_span.end.copy());
        self.make_range_struct_lit("RangeFrom", vec![("start", start)], span)
    }

    fn build_range_to(&mut self, end: Expr, dot_span: Span) -> Expr {
        let span = Span::new(dot_span.start.copy(), end.span.end.copy());
        self.make_range_struct_lit("RangeTo", vec![("end", end)], span)
    }

    fn build_range_inclusive(&mut self, start: Expr, end: Expr, _dot_span: Span) -> Expr {
        let span = Span::new(start.span.start.copy(), end.span.end.copy());
        self.make_range_struct_lit(
            "RangeInclusive",
            vec![("start", start), ("end", end)],
            span,
        )
    }

    fn build_range_to_inclusive(&mut self, end: Expr, dot_span: Span) -> Expr {
        let span = Span::new(dot_span.start.copy(), end.span.end.copy());
        self.make_range_struct_lit("RangeToInclusive", vec![("end", end)], span)
    }

    fn build_range_full(&mut self, dot_span: Span) -> Expr {
        self.make_range_struct_lit("RangeFull", Vec::new(), dot_span)
    }

    // `||` and `&&` short-circuit. Desugar at parse-time to if-else
    // expressions (`a && b` → `if a { b } else { false }`, `a || b`
    // → `if a { true } else { b }`) so semantics — including the
    // skipping of the rhs when the lhs decides the result — fall out
    // of the existing if-expr machinery. Precedence: `||` lowest, then
    // `&&`, then comparisons (matches Rust).
    fn parse_logical_or(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_logical_and()?;
        while self.peek_kind(&TokenKind::OrOr) {
            self.pos += 1;
            let rhs = self.parse_logical_and()?;
            expr = self.build_short_circuit(expr, rhs, /*is_and=*/ false);
        }
        Ok(expr)
    }

    fn parse_logical_and(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_comparison()?;
        while self.peek_kind(&TokenKind::AndAnd) {
            self.pos += 1;
            let rhs = self.parse_comparison()?;
            expr = self.build_short_circuit(expr, rhs, /*is_and=*/ true);
        }
        Ok(expr)
    }

    // Build `if lhs { rhs } else { false }` (AND) or `if lhs { true }
    // else { rhs }` (OR). Each synthesized expression gets a fresh
    // node id so typeck's per-Expr.id artifacts line up.
    fn build_short_circuit(&mut self, lhs: Expr, rhs: Expr, is_and: bool) -> Expr {
        let span = Span::new(lhs.span.start.copy(), rhs.span.end.copy());
        let bool_lit = |this: &mut Self, value: bool, sp: Span| -> Expr {
            let id = this.alloc_node_id();
            Expr {
                kind: ExprKind::BoolLit(value),
                span: sp,
                id,
            }
        };
        let mk_block = |expr: Expr, sp: Span| -> Block {
            Block {
                stmts: Vec::new(),
                tail: Some(expr),
                span: sp,
            }
        };
        let (then_expr, else_expr) = if is_and {
            (rhs, bool_lit(self, false, span.copy()))
        } else {
            (bool_lit(self, true, span.copy()), rhs)
        };
        let then_span = then_expr.span.copy();
        let else_span = else_expr.span.copy();
        let if_expr = IfExpr {
            cond: Box::new(lhs),
            then_block: Box::new(mk_block(then_expr, then_span)),
            else_block: Box::new(mk_block(else_expr, else_span)),
        };
        let id = self.alloc_node_id();
        Expr {
            kind: ExprKind::If(if_expr),
            span,
            id,
        }
    }

    // Comparison ops are non-associative (precedence layer right above
    // additive). Parsing `a < b < c` would give an error in real Rust;
    // here we accept left-associativity for simplicity (`(a < b) < c`).
    fn parse_comparison(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_additive()?;
        loop {
            let method_name: &str = if self.peek_kind(&TokenKind::EqEq) {
                "eq"
            } else if self.peek_kind(&TokenKind::NotEq) {
                "ne"
            } else if self.peek_kind(&TokenKind::LAngle) {
                "lt"
            } else if self.peek_kind(&TokenKind::LtEq) {
                "le"
            } else if self.peek_kind(&TokenKind::RAngle) {
                "gt"
            } else if self.peek_kind(&TokenKind::GtEq) {
                "ge"
            } else {
                break;
            };
            let op_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let rhs = self.parse_additive()?;
            // Comparison methods take `&self, &Self`. Wrap rhs in a
            // borrow; receiver autorefs via method dispatch.
            let rhs_span = rhs.span.copy();
            let rhs_id = self.alloc_node_id();
            let rhs_borrow = Expr {
                kind: ExprKind::Borrow {
                    inner: Box::new(rhs),
                    mutable: false,
                },
                span: rhs_span,
                id: rhs_id,
            };
            let span = Span::new(expr.span.start.copy(), rhs_borrow.span.end.copy());
            let id = self.alloc_node_id();
            expr = Expr {
                kind: ExprKind::MethodCall(MethodCall {
                    receiver: Box::new(expr),
                    method: method_name.to_string(),
                    method_span: op_span,
                    turbofish_args: Vec::new(),
                    args: vec![rhs_borrow],
                }),
                span,
                id,
            };
        }
        Ok(expr)
    }

    fn parse_additive(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_multiplicative()?;
        loop {
            let method_name: &str = if self.peek_kind(&TokenKind::Plus) {
                "add"
            } else if self.peek_kind(&TokenKind::Minus) {
                "sub"
            } else {
                break;
            };
            let op_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let rhs = self.parse_multiplicative()?;
            let span = Span::new(expr.span.start.copy(), rhs.span.end.copy());
            let id = self.alloc_node_id();
            expr = Expr {
                kind: ExprKind::MethodCall(MethodCall {
                    receiver: Box::new(expr),
                    method: method_name.to_string(),
                    method_span: op_span,
                    turbofish_args: Vec::new(),
                    args: vec![rhs],
                }),
                span,
                id,
            };
        }
        Ok(expr)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_cast()?;
        loop {
            let method_name: &str = if self.peek_kind(&TokenKind::Star) {
                "mul"
            } else if self.peek_kind(&TokenKind::Slash) {
                "div"
            } else if self.peek_kind(&TokenKind::Percent) {
                "rem"
            } else {
                break;
            };
            let op_span = self.tokens[self.pos].span.copy();
            self.pos += 1;
            let rhs = self.parse_cast()?;
            let span = Span::new(expr.span.start.copy(), rhs.span.end.copy());
            let id = self.alloc_node_id();
            expr = Expr {
                kind: ExprKind::MethodCall(MethodCall {
                    receiver: Box::new(expr),
                    method: method_name.to_string(),
                    method_span: op_span,
                    turbofish_args: Vec::new(),
                    args: vec![rhs],
                }),
                span,
                id,
            };
        }
        Ok(expr)
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
        // Unary minus. Two cases:
        //   - `-INT_LIT` collapses to a single `NegIntLit(value)`
        //     where `value` is the absolute magnitude. This is what
        //     makes `i32::MIN` expressible: `-2147483648` is in
        //     `i32`'s range, but `2147483648` (the magnitude alone)
        //     overflows `i32`'s positive range, so the
        //     `2147483648.neg()` desugar would fail the literal's
        //     range check before `neg` ever ran. `NegIntLit` carries
        //     the negative sign through inference so the body-end
        //     range check sees `-2147483648` against `i32`'s full
        //     range.
        //   - `-other_expr` desugars to `other_expr.neg()` —
        //     `VecSpace::neg`, of which `Num` is a subtrait, so
        //     every numeric kind plus any user `impl VecSpace` works.
        // Bound tighter than additive (`-a + b` parses as `(-a) + b`).
        if self.peek_kind(&TokenKind::Minus) {
            let op_span = self.expect(&TokenKind::Minus, "`-`")?;
            // Special case `-INT_LIT`.
            if self.pos < self.tokens.len() {
                if let TokenKind::IntLit(n) = &self.tokens[self.pos].kind {
                    let value = *n;
                    let lit_span = self.tokens[self.pos].span.copy();
                    self.pos += 1;
                    let span = Span::new(op_span.start.copy(), lit_span.end.copy());
                    let id = self.alloc_node_id();
                    return Ok(Expr {
                        kind: ExprKind::NegIntLit(value),
                        span,
                        id,
                    });
                }
            }
            let inner = self.parse_unary()?;
            let span = Span::new(op_span.start.copy(), inner.span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Expr {
                kind: ExprKind::MethodCall(MethodCall {
                    receiver: Box::new(inner),
                    method: "neg".to_string(),
                    method_span: op_span,
                    turbofish_args: Vec::new(),
                    args: Vec::new(),
                }),
                span,
                id,
            });
        }
        // Prefix `!` — boolean / bitwise NOT. Desugars to `inner.not()`,
        // dispatched via `std::ops::Not`. Distinguished from the
        // `name!(args)` macro form by macro detection happening in
        // `parse_path_atom` which only fires when the `!` follows an
        // ident-path; bare `!` here is always a unary operator.
        if self.peek_kind(&TokenKind::Bang) {
            let op_span = self.expect(&TokenKind::Bang, "`!`")?;
            let inner = self.parse_unary()?;
            let span = Span::new(op_span.start.copy(), inner.span.end.copy());
            let id = self.alloc_node_id();
            return Ok(Expr {
                kind: ExprKind::MethodCall(MethodCall {
                    receiver: Box::new(inner),
                    method: "not".to_string(),
                    method_span: op_span,
                    turbofish_args: Vec::new(),
                    args: Vec::new(),
                }),
                span,
                id,
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_atom()?;
        // Postfix loop covering `.field` / `.method(...)` / `.tuple_idx`,
        // the `?` try operator, and `[idx]` indexing. All three share
        // the same precedence level — bind tighter than any prefix or
        // binary operator.
        while self.peek_kind(&TokenKind::Dot)
            || self.peek_kind(&TokenKind::Question)
            || self.peek_kind(&TokenKind::LBracket)
        {
            if self.peek_kind(&TokenKind::Question) {
                let q_span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let span = Span::new(expr.span.start.copy(), q_span.end.copy());
                let id = self.alloc_node_id();
                expr = Expr {
                    kind: ExprKind::Try {
                        inner: Box::new(expr),
                        question_span: q_span,
                    },
                    span,
                    id,
                };
                continue;
            }
            if self.peek_kind(&TokenKind::LBracket) {
                let lb = self.expect(&TokenKind::LBracket, "`[`")?;
                let saved = self.no_struct_lit;
                self.no_struct_lit = false;
                let idx_expr = self.parse_expr()?;
                self.no_struct_lit = saved;
                let rb = self.expect(&TokenKind::RBracket, "`]`")?;
                let span = Span::new(expr.span.start.copy(), rb.end.copy());
                let id = self.alloc_node_id();
                expr = Expr {
                    kind: ExprKind::Index {
                        base: Box::new(expr),
                        index: Box::new(idx_expr),
                        bracket_span: Span::new(lb.start, rb.end),
                    },
                    span,
                    id,
                };
                continue;
            }
            self.pos += 1;
            // `.<integer>` — tuple-index access. We accept any non-
            // negative integer literal here; range-checks (against the
            // tuple's arity) happen in typeck.
            if self.pos < self.tokens.len() {
                if let TokenKind::IntLit(n) = &self.tokens[self.pos].kind {
                    let n = *n;
                    let index_span = self.tokens[self.pos].span.copy();
                    if n > u32::MAX as u64 {
                        return Err(Error {
                            file: self.file.clone(),
                            message: "tuple index too large".to_string(),
                            span: index_span,
                        });
                    }
                    self.pos += 1;
                    let span = Span::new(expr.span.start.copy(), index_span.end.copy());
                    let id = self.alloc_node_id();
                    expr = Expr {
                        kind: ExprKind::TupleIndex {
                            base: Box::new(expr),
                            index: n as u32,
                            index_span,
                        },
                        span,
                        id,
                    };
                    continue;
                }
            }
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

    // Closure expression: `|p1, p2| body`, `|p: T| body`, `|| body`,
    // `move |...| body`. Each param is `name` or `name: T` (no patterns
    // yet — single-binding only); both arg-type annotations and the
    // body's `-> R` annotation are optional. With explicit `-> R`, the
    // body must be a brace block. The body extends right via parse_expr,
    // so `|x| x + 1 + 2` reads as `|x| (x + 1 + 2)`.
    fn parse_closure(&mut self) -> Result<Expr, Error> {
        let start_span = self.tokens[self.pos].span.copy();
        let is_move = if self.peek_kind(&TokenKind::Move) {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut params: Vec<ClosureParam> = Vec::new();
        if self.peek_kind(&TokenKind::OrOr) {
            // Empty arg list — `||` is one token here.
            self.pos += 1;
        } else {
            self.expect(&TokenKind::Pipe, "`|`")?;
            if !self.peek_kind(&TokenKind::Pipe) {
                params.push(self.parse_closure_param()?);
                while self.peek_kind(&TokenKind::Comma) {
                    self.pos += 1;
                    if self.peek_kind(&TokenKind::Pipe) {
                        break;
                    }
                    params.push(self.parse_closure_param()?);
                }
            }
            self.expect(&TokenKind::Pipe, "`|`")?;
        }
        let return_type = if self.peek_kind(&TokenKind::Arrow) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        // With an explicit `-> R`, Rust requires a brace block body so
        // there's no precedence question about where the body ends.
        let body = if return_type.is_some() {
            if !self.peek_kind(&TokenKind::LBrace) {
                let span = if self.pos < self.tokens.len() {
                    self.tokens[self.pos].span.copy()
                } else {
                    self.eof_span()
                };
                return Err(Error {
                    file: self.file.clone(),
                    message: "closure body must be a `{ … }` block when an explicit `-> R` return type is given".to_string(),
                    span,
                });
            }
            self.parse_block_expr()?
        } else {
            self.parse_expr()?
        };
        let end = body.span.end.copy();
        let span = Span::new(start_span.start.copy(), end);
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Closure(Closure {
                params,
                return_type,
                body: Box::new(body),
                is_move,
                span: span.copy(),
            }),
            span,
            id,
        })
    }

    fn parse_closure_param(&mut self) -> Result<ClosureParam, Error> {
        // `|` delimits the param list, so or-patterns can't appear at
        // the top level of a closure param. `parse_pattern_no_or`
        // handles `name`, `_`, `(a, b)`, `Foo { x, y }`, `&pat`, …
        let pattern = self.parse_pattern_no_or()?;
        let ty = if self.peek_kind(&TokenKind::Colon) {
            self.pos += 1;
            Some(self.parse_type()?)
        } else {
            None
        };
        Ok(ClosureParam { pattern, ty })
    }

    fn parse_atom(&mut self) -> Result<Expr, Error> {
        if self.pos >= self.tokens.len() {
            return Err(Error {
                file: self.file.clone(),
                message: "expected expression, got end of input".to_string(),
                span: self.eof_span(),
            });
        }
        // Closure expressions: `|args| body`, `move |args| body`, `||
        // body`, `move || body`. Detected by leading `|` (Pipe) or `||`
        // (OrOr) — `OrOr` only at the start of an atom can't be the
        // logical-or operator (which always has a left operand). The
        // `move` keyword is a closure prefix here.
        if matches!(
            &self.tokens[self.pos].kind,
            TokenKind::Pipe | TokenKind::OrOr | TokenKind::Move
        ) {
            return self.parse_closure();
        }
        match &self.tokens[self.pos].kind {
            TokenKind::IntLit(_) => self.parse_int_lit(),
            TokenKind::StrLit(_) => self.parse_str_lit(),
            TokenKind::CharLit(c) => {
                let value = *c;
                let span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let id = self.alloc_node_id();
                Ok(Expr {
                    kind: ExprKind::CharLit(value),
                    span,
                    id,
                })
            }
            TokenKind::True => {
                let span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let id = self.alloc_node_id();
                Ok(Expr { kind: ExprKind::BoolLit(true), span, id })
            }
            TokenKind::False => {
                let span = self.tokens[self.pos].span.copy();
                self.pos += 1;
                let id = self.alloc_node_id();
                Ok(Expr { kind: ExprKind::BoolLit(false), span, id })
            }
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::While => self.parse_while_expr(None, None),
            TokenKind::For => self.parse_for_expr(None, None),
            TokenKind::Break => self.parse_break_expr(),
            TokenKind::Continue => self.parse_continue_expr(),
            TokenKind::Return => self.parse_return_expr(),
            TokenKind::Lifetime(_) => self.parse_labeled_loop(),
            TokenKind::Builtin => self.parse_builtin(),
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
                let lp = self.tokens[self.pos].span.copy();
                self.pos += 1;
                // `()` — unit value (empty tuple).
                if self.peek_kind(&TokenKind::RParen) {
                    let rp = self.expect(&TokenKind::RParen, "`)`")?;
                    let id = self.alloc_node_id();
                    return Ok(Expr {
                        kind: ExprKind::Tuple(Vec::new()),
                        span: Span::new(lp.start, rp.end),
                        id,
                    });
                }
                let saved = self.no_struct_lit;
                self.no_struct_lit = false;
                let first = self.parse_expr()?;
                self.no_struct_lit = saved;
                if self.peek_kind(&TokenKind::Comma) {
                    // Tuple expression: at least one trailing comma.
                    // `(a,)` → 1-tuple, `(a, b)` → 2-tuple, etc.
                    let mut elems: Vec<Expr> = Vec::new();
                    elems.push(first);
                    while self.peek_kind(&TokenKind::Comma) {
                        self.pos += 1;
                        if self.peek_kind(&TokenKind::RParen) {
                            break;
                        }
                        let saved2 = self.no_struct_lit;
                        self.no_struct_lit = false;
                        elems.push(self.parse_expr()?);
                        self.no_struct_lit = saved2;
                    }
                    let rp = self.expect(&TokenKind::RParen, "`)`")?;
                    let id = self.alloc_node_id();
                    return Ok(Expr {
                        kind: ExprKind::Tuple(elems),
                        span: Span::new(lp.start, rp.end),
                        id,
                    });
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                Ok(first)
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


    // `if cond { … } else { … }` — `else` is optional: `if cond { … }`
    // implicitly elses to an empty block (unit-typed). Chained `else if`
    // desugars to a block whose tail is the chained if expression.
    // Struct literals are disallowed in the condition; wrap in parens
    // to use them.
    fn parse_if_expr(&mut self) -> Result<Expr, Error> {
        let if_span = self.expect(&TokenKind::If, "`if`")?;
        // `if let Pat = scrut { ... } else { ... }` — first-class form,
        // not desugared to `match`.
        if self.peek_kind(&TokenKind::Let) {
            self.pos += 1;
            let pattern = self.parse_pattern()?;
            self.expect(&TokenKind::Eq, "`=`")?;
            let saved = self.no_struct_lit;
            self.no_struct_lit = true;
            let scrutinee = self.parse_expr()?;
            self.no_struct_lit = saved;
            let then_block = self.parse_block()?;
            let else_block = self.parse_else_or_empty(&then_block)?;
            let end = else_block.span.end.copy();
            let span = Span::new(if_span.start, end);
            let id = self.alloc_node_id();
            return Ok(Expr {
                kind: ExprKind::IfLet(IfLetExpr {
                    pattern,
                    scrutinee: Box::new(scrutinee),
                    then_block: Box::new(then_block),
                    else_block: Box::new(else_block),
                }),
                span,
                id,
            });
        }
        let saved = self.no_struct_lit;
        self.no_struct_lit = true;
        let cond = self.parse_expr()?;
        self.no_struct_lit = saved;
        let then_block = self.parse_block()?;
        let else_block = self.parse_else_or_empty(&then_block)?;
        let end = else_block.span.end.copy();
        let span = Span::new(if_span.start, end);
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::If(IfExpr {
                cond: Box::new(cond),
                then_block: Box::new(then_block),
                else_block: Box::new(else_block),
            }),
            span,
            id,
        })
    }

    // Shared between `if` and `if let`: parse `else { ... }` /
    // `else if ... { ... }` / `else if let ... { ... }`, or synthesize
    // an empty block when no `else` is present.
    fn parse_else_or_empty(&mut self, then_block: &Block) -> Result<Block, Error> {
        if self.peek_kind(&TokenKind::Else) {
            self.pos += 1;
            if self.peek_kind(&TokenKind::If) {
                let chained = self.parse_if_expr()?;
                let span = chained.span.copy();
                Ok(Block {
                    stmts: Vec::new(),
                    tail: Some(chained),
                    span,
                })
            } else {
                self.parse_block()
            }
        } else {
            let end = then_block.span.end.copy();
            Ok(Block {
                stmts: Vec::new(),
                tail: None,
                span: Span::new(end.copy(), end),
            })
        }
    }

    // `match scrut { pat => arm, pat if guard => arm, _ => arm, ... }`.
    // Trailing comma after the last arm is optional, and arms whose body
    // is a brace-block may omit the trailing comma.
    fn parse_match_expr(&mut self) -> Result<Expr, Error> {
        let match_span = self.expect(&TokenKind::Match, "`match`")?;
        let saved = self.no_struct_lit;
        self.no_struct_lit = true;
        let scrutinee = self.parse_expr()?;
        self.no_struct_lit = saved;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut arms: Vec<MatchArm> = Vec::new();
        while !self.peek_kind(&TokenKind::RBrace) {
            arms.push(self.parse_match_arm()?);
            if self.peek_kind(&TokenKind::Comma) {
                self.pos += 1;
            } else if !self.peek_kind(&TokenKind::RBrace) {
                // No comma and not at end — only allowed when the arm
                // body is a brace-block (then comma is optional).
                let last = arms.last().unwrap();
                if !is_brace_block_expr(&last.body) {
                    return Err(self.error_at_current("expected `,` after match arm"));
                }
            }
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        let span = Span::new(match_span.start, rb.end);
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms,
                span: span.copy(),
            }),
            span,
            id,
        })
    }

    fn parse_match_arm(&mut self) -> Result<MatchArm, Error> {
        let pattern = self.parse_pattern()?;
        let pat_start = pattern.span.start.copy();
        let guard = if self.peek_kind(&TokenKind::If) {
            self.pos += 1;
            let saved = self.no_struct_lit;
            self.no_struct_lit = true;
            let g = self.parse_expr()?;
            self.no_struct_lit = saved;
            Some(g)
        } else {
            None
        };
        self.expect(&TokenKind::FatArrow, "`=>`")?;
        let body = self.parse_expr()?;
        let span = Span::new(pat_start, body.span.end.copy());
        Ok(MatchArm {
            pattern,
            guard,
            body,
            span,
        })
    }

    // `¤name(arg, ...)` — a compiler-builtin call. The name is a
    // single identifier (no path segments); arg list is parsed like a
    // regular call. Typeck validates the name + arg shape; codegen
    // lowers to wasm ops.
    fn parse_builtin(&mut self) -> Result<Expr, Error> {
        let cur_span = self.expect(&TokenKind::Builtin, "`¤`")?;
        let (name, name_span) = self.expect_ident()?;
        // Optional turbofish: `¤name::<T1, T2>(args)`. Lifetime args
        // aren't accepted (no builtin needs them today); they're parsed
        // and dropped to keep error reporting uniform with method-call
        // turbofish.
        let type_args = if self.peek_two(&TokenKind::PathSep, &TokenKind::LAngle) {
            self.pos += 1; // skip `::`
            let (_lifetime_args, args) = self.parse_angle_args()?;
            args
        } else {
            Vec::new()
        };
        let args = self.parse_call_args()?;
        let end = self.tokens[self.pos - 1].span.end.copy();
        let span = Span::new(cur_span.start, end);
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Builtin {
                name,
                name_span,
                type_args,
                args,
            },
            span,
            id,
        })
    }

    // Parse `while cond { body }`. The condition disallows bare struct
    // literals (matches Rust's parser).
    fn parse_while_expr(
        &mut self,
        label: Option<String>,
        label_span: Option<Span>,
    ) -> Result<Expr, Error> {
        let while_span = self.expect(&TokenKind::While, "`while`")?;
        let saved = self.no_struct_lit;
        self.no_struct_lit = true;
        let cond = self.parse_expr()?;
        self.no_struct_lit = saved;
        let body = self.parse_block()?;
        let span_start = label_span
            .as_ref()
            .map(|s| s.start.copy())
            .unwrap_or(while_span.start);
        let span = Span::new(span_start, body.span.end.copy());
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::While(crate::ast::WhileExpr {
                label,
                label_span,
                cond: Box::new(cond),
                body: Box::new(body),
            }),
            span,
            id,
        })
    }

    // `'label: <loop>` — supports `'label: while …` and
    // `'label: for …`. The label is then targetable by `break 'label`
    // / `continue 'label` from inside the body.
    fn parse_labeled_loop(&mut self) -> Result<Expr, Error> {
        let (name, label_span) = self.expect_lifetime()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        if self.peek_kind(&TokenKind::While) {
            self.parse_while_expr(Some(name), Some(label_span))
        } else if self.peek_kind(&TokenKind::For) {
            self.parse_for_expr(Some(name), Some(label_span))
        } else {
            Err(self.error_at_current(
                "expected `while` or `for` after label",
            ))
        }
    }

    // `for pat in iter { body }` — iterates `iter` (which must impl
    // `Iterator`) by repeatedly calling `Iterator::next(&mut iter)`
    // until `None`. The pattern binds the `Some` payload's value
    // each iteration. Loop's expression-type is `()`.
    fn parse_for_expr(
        &mut self,
        label: Option<String>,
        label_span: Option<Span>,
    ) -> Result<Expr, Error> {
        let for_span = self.expect(&TokenKind::For, "`for`")?;
        let pattern = self.parse_pattern()?;
        self.expect(&TokenKind::In, "`in`")?;
        let saved = self.no_struct_lit;
        self.no_struct_lit = true;
        let iter = self.parse_expr()?;
        self.no_struct_lit = saved;
        let body = self.parse_block()?;
        let span_start = label_span
            .as_ref()
            .map(|s| s.start.copy())
            .unwrap_or(for_span.start);
        let span = Span::new(span_start, body.span.end.copy());
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::For(crate::ast::ForLoop {
                label,
                label_span,
                pattern,
                iter: Box::new(iter),
                body: Box::new(body),
            }),
            span,
            id,
        })
    }

    fn parse_break_expr(&mut self) -> Result<Expr, Error> {
        let kw_span = self.expect(&TokenKind::Break, "`break`")?;
        let (label, label_span, end) = if self.peek_lifetime() {
            let (n, ls) = self.expect_lifetime()?;
            let end = ls.end.copy();
            (Some(n), Some(ls), end)
        } else {
            (None, None, kw_span.end.copy())
        };
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Break { label, label_span },
            span: Span::new(kw_span.start, end),
            id,
        })
    }

    fn parse_continue_expr(&mut self) -> Result<Expr, Error> {
        let kw_span = self.expect(&TokenKind::Continue, "`continue`")?;
        let (label, label_span, end) = if self.peek_lifetime() {
            let (n, ls) = self.expect_lifetime()?;
            let end = ls.end.copy();
            (Some(n), Some(ls), end)
        } else {
            (None, None, kw_span.end.copy())
        };
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Continue { label, label_span },
            span: Span::new(kw_span.start, end),
            id,
        })
    }

    fn parse_return_expr(&mut self) -> Result<Expr, Error> {
        let kw_span = self.expect(&TokenKind::Return, "`return`")?;
        // The value is optional. We consider `return` to have no
        // value when followed by a token that can't start an
        // expression: `;`, `,`, `)`, `]`, `}`, `=>`. Anything else
        // we try to parse as an expression.
        let has_value = if self.pos >= self.tokens.len() {
            false
        } else {
            !matches!(
                &self.tokens[self.pos].kind,
                TokenKind::Semi
                    | TokenKind::Comma
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::FatArrow
            )
        };
        let (value, end) = if has_value {
            let e = self.parse_expr()?;
            let end = e.span.end.copy();
            (Some(Box::new(e)), end)
        } else {
            (None, kw_span.end.copy())
        };
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Return { value },
            span: Span::new(kw_span.start, end),
            id,
        })
    }

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
                let lit = Expr {
                    kind: ExprKind::IntLit(value),
                    span: span.copy(),
                    id,
                };
                // Optional type suffix: `42u32`, `100i64`, etc. The
                // lexer reads digits then stops at any non-digit, so
                // `42u32` arrives as `IntLit(42)` followed by `Ident("u32")`.
                // If the next token is one of the recognized integer-
                // kind names, desugar to `(lit as <kind>)` — the
                // existing cast machinery pins the literal's type at
                // typeck and emits no runtime work for same-class
                // conversions.
                if let Some(suffix) = self.peek_int_suffix() {
                    let suffix_span = self.tokens[self.pos].span.copy();
                    self.pos += 1;
                    let cast_id = self.alloc_node_id();
                    return Ok(Expr {
                        kind: ExprKind::Cast {
                            inner: Box::new(lit),
                            ty: Type {
                                kind: TypeKind::Path(Path {
                                    segments: vec![PathSegment {
                                        name: suffix,
                                        args: Vec::new(),
                                        lifetime_args: Vec::new(),
                                        span: suffix_span.copy(),
                                    }],
                                    span: suffix_span.copy(),
                                }),
                                span: suffix_span.copy(),
                            },
                        },
                        span: Span::new(span.start, suffix_span.end),
                        id: cast_id,
                    });
                }
                Ok(lit)
            }
            _ => unreachable!(),
        }
    }

    // Peek for a literal type-suffix ident (`u8`/`i8`/.../`usize`/`isize`/
    // `u128`/`i128`). Returns the suffix name if present.
    fn peek_int_suffix(&self) -> Option<String> {
        if self.pos >= self.tokens.len() {
            return None;
        }
        if let TokenKind::Ident(name) = &self.tokens[self.pos].kind {
            match name.as_str() {
                "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "u64" | "i64"
                | "u128" | "i128" | "usize" | "isize" => Some(name.clone()),
                _ => None,
            }
        } else {
            None
        }
    }

    fn parse_str_lit(&mut self) -> Result<Expr, Error> {
        let tok = &self.tokens[self.pos];
        match &tok.kind {
            TokenKind::StrLit(s) => {
                let payload = s.clone();
                let span = tok.span.copy();
                self.pos += 1;
                let id = self.alloc_node_id();
                Ok(Expr {
                    kind: ExprKind::StrLit(payload),
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
        // `name!(args)` / `name![args]` — macro invocation. Single-
        // segment path followed by `!` and an open delimiter. Both
        // bracket forms parse identical args (comma-separated exprs);
        // `[]` is what `vec![…]` uses.
        let macro_paren = path.segments.len() == 1
            && !had_turbofish
            && self.peek_two(&TokenKind::Bang, &TokenKind::LParen);
        let macro_bracket = path.segments.len() == 1
            && !had_turbofish
            && self.peek_two(&TokenKind::Bang, &TokenKind::LBracket);
        if macro_paren || macro_bracket {
            let bang_span = self.expect(&TokenKind::Bang, "`!`")?;
            let _ = bang_span;
            // `matches!(scrutinee, pattern)` and `matches!(scrutinee,
            // pattern if guard)` — args don't fit the expression-list
            // shape (the second arg is a *pattern*, not an expr), so
            // parse them by hand and desugar at parse time to a
            // 2-arm `match`. Only the parens form is supported (Rust
            // accepts `[]` and `{}` too, but `matches!` is conventionally
            // written with parens; deferring the bracket forms).
            if path.segments[0].name == "matches" && macro_paren {
                let span_start = path.span.start.copy();
                return self.parse_matches_macro(span_start);
            }
            let args = if macro_paren {
                self.parse_call_args()?
            } else {
                self.parse_macro_bracket_args()?
            };
            let end = self.tokens[self.pos - 1].span.end.copy();
            let span = Span::new(path.span.start.copy(), end);
            // `vec![a, b, c]` desugars to a block expression at parse
            // time: `{ let mut __pr_vec_<id> = Vec::new(); __v.push(a);
            // __v.push(b); __v.push(c); __v }`. The block's value is
            // the freshly built Vec. Empty `vec![]` is just
            // `Vec::new()` (the let + tail still produce that). Type
            // inference fills T from the pushed elements (or from
            // surrounding context if `vec![]`).
            if path.segments[0].name == "vec" {
                return Ok(self.desugar_vec_macro(args, span));
            }
            let id = self.alloc_node_id();
            return Ok(Expr {
                kind: ExprKind::MacroCall {
                    name: path.segments[0].name.clone(),
                    name_span: path.segments[0].span.copy(),
                    args,
                },
                span,
                id,
            });
        }
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
        } else if self.peek_kind(&TokenKind::LBrace) && !self.no_struct_lit {
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
            // Bare multi-segment path with no `(` or `{` after — could
            // be a unit-variant reference (`Option::None`) or a path
            // to a free function used as a value (no first-class
            // function values yet, so this latter case currently errors
            // at typeck). Route through Call with an empty arg list;
            // typeck disambiguates via the enum table.
            let span = path.span.copy();
            let id = self.alloc_node_id();
            Ok(Expr {
                kind: ExprKind::Call(Call {
                    callee: path,
                    args: Vec::new(),
                }),
                span,
                id,
            })
        }
    }

    fn peek_two(&self, a: &TokenKind, b: &TokenKind) -> bool {
        self.pos + 1 < self.tokens.len()
            && Self::kind_eq(&self.tokens[self.pos].kind, a)
            && Self::kind_eq(&self.tokens[self.pos + 1].kind, b)
    }

    // `matches!(scrutinee, pattern)` / `matches!(scrutinee, pattern
    // if guard)` desugars to:
    //
    //   match scrutinee {
    //       pattern (if guard)? => true,
    //       _ => false,
    //   }
    //
    // The second arg is a *pattern* (and optional guard), not an
    // expression — so we can't reuse `parse_call_args`; this is a
    // hand-rolled parser that calls `parse_pattern` for the pattern
    // slot. Only the `(…)` delimiter form is wired up; `[…]`/`{…}`
    // for `matches!` are deferred since the canonical Rust spelling
    // is parens.
    fn parse_matches_macro(&mut self, span_start: crate::span::Pos) -> Result<Expr, Error> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let scrutinee = self.parse_expr()?;
        self.expect(&TokenKind::Comma, "`,`")?;
        let pattern = self.parse_pattern()?;
        let pat_span = pattern.span.copy();
        let guard = if self.peek_kind(&TokenKind::If) {
            self.pos += 1;
            let saved = self.no_struct_lit;
            self.no_struct_lit = true;
            let g = self.parse_expr()?;
            self.no_struct_lit = saved;
            Some(g)
        } else {
            None
        };
        let rparen = self.expect(&TokenKind::RParen, "`)`")?;
        let span = Span::new(span_start, rparen.end.copy());
        // Build the two arms: `pattern (if guard)? => true,` and
        // `_ => false,`.
        let true_id = self.alloc_node_id();
        let false_id = self.alloc_node_id();
        let true_expr = Expr {
            kind: ExprKind::BoolLit(true),
            span: span.copy(),
            id: true_id,
        };
        let false_expr = Expr {
            kind: ExprKind::BoolLit(false),
            span: span.copy(),
            id: false_id,
        };
        let body_span_a = pat_span.copy();
        let arm_a = MatchArm {
            pattern,
            guard,
            body: true_expr,
            span: body_span_a,
        };
        let wildcard_pat = Pattern {
            kind: crate::ast::PatternKind::Wildcard,
            span: span.copy(),
            id: self.alloc_node_id(),
        };
        let arm_b = MatchArm {
            pattern: wildcard_pat,
            guard: None,
            body: false_expr,
            span: span.copy(),
        };
        let id = self.alloc_node_id();
        Ok(Expr {
            kind: ExprKind::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms: vec![arm_a, arm_b],
                span: span.copy(),
            }),
            span,
            id,
        })
    }

    fn parse_macro_bracket_args(&mut self) -> Result<Vec<Expr>, Error> {
        self.expect(&TokenKind::LBracket, "`[`")?;
        if self.peek_kind(&TokenKind::RBracket) {
            self.pos += 1;
            return Ok(Vec::new());
        }
        let mut args = Vec::new();
        args.push(self.parse_expr()?);
        while self.peek_kind(&TokenKind::Comma) {
            self.pos += 1;
            if self.peek_kind(&TokenKind::RBracket) {
                break;
            }
            args.push(self.parse_expr()?);
        }
        self.expect(&TokenKind::RBracket, "`]`")?;
        Ok(args)
    }

    // Build the AST for `{ let mut <var> = Vec::new(); <var>.push(a);
    // <var>.push(b); …; <var> }`. All freshly synthesized expressions
    // get fresh node IDs so typeck's per-Expr.id artifacts (expr_types,
    // method_resolutions, …) line up. The temporary's name is
    // disambiguated with the outer macro's node id so multiple
    // `vec![…]` invocations in the same function don't shadow each
    // other.
    fn desugar_vec_macro(&mut self, args: Vec<Expr>, span: Span) -> Expr {
        let outer_id = self.alloc_node_id();
        let var_name = format!("__pr_vec_{}", outer_id);
        let mk_var = |this: &mut Self| -> Expr {
            let id = this.alloc_node_id();
            Expr {
                kind: ExprKind::Var(var_name.clone()),
                span: span.copy(),
                id,
            }
        };
        // `Vec::new()` — Call with two-segment path.
        let new_call = {
            let id = self.alloc_node_id();
            let path = Path {
                segments: vec![
                    PathSegment {
                        name: "Vec".to_string(),
                        span: span.copy(),
                        lifetime_args: Vec::new(),
                        args: Vec::new(),
                    },
                    PathSegment {
                        name: "new".to_string(),
                        span: span.copy(),
                        lifetime_args: Vec::new(),
                        args: Vec::new(),
                    },
                ],
                span: span.copy(),
            };
            Expr {
                kind: ExprKind::Call(Call {
                    callee: path,
                    args: Vec::new(),
                }),
                span: span.copy(),
                id,
            }
        };
        // Synthetic `let mut __pr_vec_<id> = Vec::new();` —
        // construct a `Pattern::Binding` with mutable=true.
        let pat_id = self.alloc_node_id();
        let pat = Pattern {
            kind: PatternKind::Binding {
                name: var_name.clone(),
                name_span: span.copy(),
                by_ref: false,
                mutable: true,
            },
            span: span.copy(),
            id: pat_id,
        };
        let let_stmt = Stmt::Let(LetStmt {
            pattern: pat,
            ty: None,
            value: Some(new_call),
            else_block: None,
        });
        let mut stmts: Vec<Stmt> = vec![let_stmt];
        let mut i = 0;
        while i < args.len() {
            let recv = mk_var(self);
            let push_id = self.alloc_node_id();
            let push_call = Expr {
                kind: ExprKind::MethodCall(MethodCall {
                    receiver: Box::new(recv),
                    method: "push".to_string(),
                    method_span: span.copy(),
                    turbofish_args: Vec::new(),
                    args: vec![args[i].clone()],
                }),
                span: span.copy(),
                id: push_id,
            };
            stmts.push(Stmt::Expr(push_call));
            i += 1;
        }
        let tail = mk_var(self);
        let block = Block {
            stmts,
            tail: Some(tail),
            span: span.copy(),
        };
        Expr {
            kind: ExprKind::Block(Box::new(block)),
            span,
            id: outer_id,
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
        // Shorthand: `Foo { x }` desugars to `Foo { x: x }` when
        // followed by `,` or `}`. The name lookup falls through to
        // typeck via the synthetic `Var` expression.
        if self.peek_kind(&TokenKind::Comma) || self.peek_kind(&TokenKind::RBrace) {
            let value = Expr {
                kind: ExprKind::Var(name.clone()),
                span: name_span.copy(),
                id: self.alloc_node_id(),
            };
            return Ok(FieldInit {
                name,
                name_span,
                value,
            });
        }
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
            (TokenKind::LBracket, TokenKind::LBracket) => true,
            (TokenKind::RBracket, TokenKind::RBracket) => true,
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
            (TokenKind::Move, TokenKind::Move) => true,
            (TokenKind::Const, TokenKind::Const) => true,
            (TokenKind::As, TokenKind::As) => true,
            (TokenKind::Unsafe, TokenKind::Unsafe) => true,
            (TokenKind::Impl, TokenKind::Impl) => true,
            (TokenKind::Trait, TokenKind::Trait) => true,
            (TokenKind::Type, TokenKind::Type) => true,
            (TokenKind::Return, TokenKind::Return) => true,
            (TokenKind::Question, TokenKind::Question) => true,
            (TokenKind::For, TokenKind::For) => true,
            (TokenKind::In, TokenKind::In) => true,
            (TokenKind::Plus, TokenKind::Plus) => true,
            (TokenKind::SelfLower, TokenKind::SelfLower) => true,
            (TokenKind::SelfUpper, TokenKind::SelfUpper) => true,
            (TokenKind::LAngle, TokenKind::LAngle) => true,
            (TokenKind::RAngle, TokenKind::RAngle) => true,
            (TokenKind::Eq, TokenKind::Eq) => true,
            (TokenKind::If, TokenKind::If) => true,
            (TokenKind::Else, TokenKind::Else) => true,
            (TokenKind::True, TokenKind::True) => true,
            (TokenKind::False, TokenKind::False) => true,
            (TokenKind::Use, TokenKind::Use) => true,
            (TokenKind::Pub, TokenKind::Pub) => true,
            (TokenKind::Builtin, TokenKind::Builtin) => true,
            (TokenKind::Minus, TokenKind::Minus) => true,
            (TokenKind::Slash, TokenKind::Slash) => true,
            (TokenKind::Percent, TokenKind::Percent) => true,
            (TokenKind::Bang, TokenKind::Bang) => true,
            (TokenKind::EqEq, TokenKind::EqEq) => true,
            (TokenKind::PlusEq, TokenKind::PlusEq) => true,
            (TokenKind::MinusEq, TokenKind::MinusEq) => true,
            (TokenKind::StarEq, TokenKind::StarEq) => true,
            (TokenKind::SlashEq, TokenKind::SlashEq) => true,
            (TokenKind::PercentEq, TokenKind::PercentEq) => true,
            (TokenKind::AndAnd, TokenKind::AndAnd) => true,
            (TokenKind::OrOr, TokenKind::OrOr) => true,
            (TokenKind::NotEq, TokenKind::NotEq) => true,
            (TokenKind::LtEq, TokenKind::LtEq) => true,
            (TokenKind::GtEq, TokenKind::GtEq) => true,
            (TokenKind::Enum, TokenKind::Enum) => true,
            (TokenKind::Match, TokenKind::Match) => true,
            (TokenKind::Ref, TokenKind::Ref) => true,
            (TokenKind::While, TokenKind::While) => true,
            (TokenKind::Break, TokenKind::Break) => true,
            (TokenKind::Continue, TokenKind::Continue) => true,
            (TokenKind::Underscore, TokenKind::Underscore) => true,
            (TokenKind::Pipe, TokenKind::Pipe) => true,
            (TokenKind::At, TokenKind::At) => true,
            (TokenKind::Hash, TokenKind::Hash) => true,
            (TokenKind::DotDot, TokenKind::DotDot) => true,
            (TokenKind::DotDotEq, TokenKind::DotDotEq) => true,
            (TokenKind::FatArrow, TokenKind::FatArrow) => true,
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

// Expressions whose syntax already delimits them with braces and which
// can therefore sit in statement position without a trailing `;`. Their
// value (if any) is discarded by the enclosing block — codegen_expr_stmt
// emits the matching number of `drop`s. `if`/`else if` chains, `match`,
// `if let`, and any brace block are recognised here.
fn is_unit_block_like(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Block(_) => true,
        ExprKind::Unsafe(_) => true,
        ExprKind::If(_) => true,
        ExprKind::IfLet(_) => true,
        ExprKind::Match(_) => true,
        ExprKind::While(_) => true,
        ExprKind::For(_) => true,
        _ => false,
    }
}

// Match arms ending in a brace-delimited expression body don't need a
// trailing comma. This is the same set as `is_unit_block_like` plus
// nothing else — so we just reuse that.
fn is_brace_block_expr(expr: &Expr) -> bool {
    is_unit_block_like(expr)
}
