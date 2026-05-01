use crate::span::Span;

// A per-function unique identifier for AST nodes that carry typing/resolution
// info (currently only `Expr`). Allocated by the parser on a per-function
// counter, captured as `Function.node_count`. Typeck sizes its side vectors
// to `node_count` and indexes them by `Expr.id`; downstream passes look up
// directly without needing to maintain source-DFS counters.
pub type NodeId = u32;

pub struct Module {
    pub name: String,
    pub name_span: Span,
    pub source_file: String,
    pub items: Vec<Item>,
}

pub enum Item {
    Function(Function),
    Module(Module),
    Struct(StructDef),
    Enum(EnumDef),
    Impl(ImplBlock),
    Trait(TraitDef),
    Use(UseDecl),
}

// `use a::b::c;` / `use a::*;` / `use a::b as c;` / `use a::{b, c::d};`
// — one declaration produces one `UseDecl` whose `tree` is parsed as a
// recursive `UseTree`. The flat list of `(local_name, full_path)`
// imports + `glob_path`s is built lazily during typeck via
// `flatten_use_tree`.
#[derive(Clone)]
pub struct UseDecl {
    pub tree: UseTree,
    // `pub use foo::Bar;` — the imported name is itself visible to
    // outside modules (re-exported). For ordinary `use` (no `pub`),
    // the import is private to its enclosing module.
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Clone)]
pub enum UseTree {
    // `use a::b::c;` or `use a::b::c as d;`. `path` is the full path;
    // `rename` is `None` for the unrenamed form (local name = last
    // segment) or `Some("d")` for renamed.
    Leaf {
        path: Vec<String>,
        rename: Option<String>,
        span: Span,
    },
    // `use prefix::{ children };`. Each child gets `prefix` prepended
    // when flattened.
    Nested {
        prefix: Vec<String>,
        children: Vec<UseTree>,
        span: Span,
    },
    // `use path::*;` — wildcard. Brings every item directly under
    // `path` into scope, resolved lazily at lookup time.
    Glob { path: Vec<String>, span: Span },
}

#[derive(Clone)]
pub struct ImplBlock {
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    // None for inherent impls (`impl Foo { ... }`); Some for trait impls
    // (`impl Trait for Foo { ... }`). The path may carry generic args, e.g.
    // `impl Show<i32> for Foo` (deferred — for now only path-only traits).
    pub trait_path: Option<Path>,
    // The type the impl applies to. For inherent impls this is restricted
    // to a struct path (`Foo<T>`); for trait impls any type pattern is
    // allowed (e.g., `&T`, `*const T`).
    pub target: Type,
    pub methods: Vec<Function>,
    pub span: Span,
}

#[derive(Clone)]
pub struct TraitDef {
    pub name: String,
    pub name_span: Span,
    pub supertraits: Vec<TraitBound>,
    pub methods: Vec<TraitMethodSig>,
    pub span: Span,
    pub is_pub: bool,
}

#[derive(Clone)]
pub struct TraitMethodSig {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
}

#[derive(Clone)]
pub struct TraitBound {
    pub path: Path,
}

#[derive(Clone)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub fields: Vec<StructField>,
    pub is_pub: bool,
}

#[derive(Clone)]
pub struct StructField {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
    pub is_pub: bool,
}

// `enum NAME<'a, T> { Variant1, Variant2(T1, T2), Variant3 { f: T } }`.
// Variant names share the enum's namespace (so two variants in the same
// enum can't share a name; two different enums may both have `Some`).
// Generic params appear on the enum, not per-variant. Variants' visibility
// inherits from the enum's `is_pub` (no per-variant `pub` modifier).
#[derive(Clone)]
pub struct EnumDef {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub variants: Vec<EnumVariant>,
    pub is_pub: bool,
}

#[derive(Clone)]
pub struct EnumVariant {
    pub name: String,
    pub name_span: Span,
    pub payload: VariantPayload,
}

#[derive(Clone)]
pub enum VariantPayload {
    // `A` — payload-less variant.
    Unit,
    // `A(T1, T2)` — positional fields. Indexed by 0/1/… in patterns.
    Tuple(Vec<Type>),
    // `A { f: T1, g: T2 }` — named fields. Layout matches a struct.
    Struct(Vec<StructField>),
}

#[derive(Clone)]
pub struct Function {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Block,
    // Number of NodeIds allocated within this function's body. Side vectors
    // (typeck/borrowck/codegen) sized to this length, indexed by Expr.id.
    pub node_count: u32,
    pub is_pub: bool,
}

#[derive(Clone)]
pub struct TypeParam {
    pub name: String,
    pub name_span: Span,
    // Trait bounds attached to this type param (e.g. `<T: Show + Eq>`).
    // Empty when no bounds were written.
    pub bounds: Vec<TraitBound>,
}

#[derive(Clone)]
pub struct LifetimeParam {
    pub name: String,
    pub name_span: Span,
}

#[derive(Clone)]
pub struct Lifetime {
    pub name: String,
    pub span: Span,
}

#[derive(Clone)]
pub struct Param {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
}

#[derive(Clone)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone)]
pub enum TypeKind {
    Path(Path),
    Ref {
        inner: Box<Type>,
        mutable: bool,
        // None when the lifetime is elided. Resolved to a concrete
        // `LifetimeRepr` during typeck (named-in-scope or fresh-inferred).
        lifetime: Option<Lifetime>,
    },
    RawPtr { inner: Box<Type>, mutable: bool },
    SelfType,
    Tuple(Vec<Type>),
}

#[derive(Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Expr>,
    pub span: Span,
}

#[derive(Clone)]
pub enum Stmt {
    Let(LetStmt),
    Assign(AssignStmt),
    Expr(Expr),
    // `use a::b::c;` inside a function body or inner block. Scoped to
    // the enclosing block — visible from the use-stmt's position to the
    // end of the block.
    Use(UseDecl),
}

#[derive(Clone)]
pub struct LetStmt {
    pub name: String,
    pub name_span: Span,
    pub mutable: bool,
    pub ty: Option<Type>,
    pub value: Expr,
}

#[derive(Clone)]
pub struct AssignStmt {
    pub lhs: Expr,
    pub rhs: Expr,
    pub span: Span,
}

#[derive(Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub id: NodeId,
}

#[derive(Clone)]
pub enum ExprKind {
    IntLit(u64),
    BoolLit(bool),
    Call(Call),
    Var(String),
    StructLit(StructLit),
    FieldAccess(FieldAccess),
    Borrow { inner: Box<Expr>, mutable: bool },
    Cast { inner: Box<Expr>, ty: Type },
    Deref(Box<Expr>),
    Unsafe(Box<Block>),
    Block(Box<Block>),
    MethodCall(MethodCall),
    If(IfExpr),
    // Tuple expression. `()` is the unit value (empty tuple); `(a,)`
    // is a 1-tuple; `(a, b)` is a 2-tuple; etc. Parenthesized
    // single-expression `(a)` is parsed as the inner expression
    // directly (no Tuple node).
    Tuple(Vec<Expr>),
    // `t.<index>` — tuple field access by 0-based numeric index.
    // Distinct from `FieldAccess` (which carries a string name) so
    // typeck/codegen can treat them separately. Valid only on tuple
    // values.
    TupleIndex {
        base: Box<Expr>,
        index: u32,
        index_span: Span,
    },
    // `¤name(args)` — a compiler-builtin intrinsic call. The name
    // identifies which primitive op (e.g. `u32_add`, `i64_eq`,
    // `bool_and`) and determines the expected arg types and result
    // type at typeck-time. Codegen lowers each builtin to a small
    // sequence of wasm instructions.
    Builtin {
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    // `match scrut { pat1 => arm1, pat2 if guard => arm2, _ => arm3 }`.
    // Arms are tried in source order; exhaustiveness is checked at
    // typeck. Guards aren't covered yet (to keep E0/E2 small) — the
    // AST has the slot reserved for when we add them.
    Match(MatchExpr),
    // `if let Pat = scrut { ... } else { ... }`. First-class node (not
    // desugared) — keeping the surface form lets typeck/codegen produce
    // better diagnostics. `else` is optional; an absent else acts like
    // an empty block (unit-typed), same as bare `if`.
    IfLet(IfLetExpr),
}

// Variant construction reuses the existing nodes:
//   `E::A`           → Call with no args (unit variant) or path-Var
//                      (a path-only `Var` when no `()` follows)
//   `E::A(x, y)`     → Call with args (tuple variant)
//   `E::A { f: e }`  → StructLit (struct variant)
// Typeck disambiguates against the active enum/struct/function tables.

#[derive(Clone)]
pub struct IfExpr {
    pub cond: Box<Expr>,
    pub then_block: Box<Block>,
    pub else_block: Box<Block>,
}

#[derive(Clone)]
pub struct MethodCall {
    pub receiver: Box<Expr>,
    pub method: String,
    pub method_span: Span,
    pub turbofish_args: Vec<Type>,
    pub args: Vec<Expr>,
}

#[derive(Clone)]
pub struct Call {
    pub callee: Path,
    pub args: Vec<Expr>,
}

#[derive(Clone)]
pub struct StructLit {
    pub path: Path,
    pub fields: Vec<FieldInit>,
}

#[derive(Clone)]
pub struct FieldInit {
    pub name: String,
    pub name_span: Span,
    pub value: Expr,
}

#[derive(Clone)]
pub struct FieldAccess {
    pub base: Box<Expr>,
    pub field: String,
    pub field_span: Span,
}

#[derive(Clone)]
pub struct Path {
    pub segments: Vec<PathSegment>,
    pub span: Span,
}

#[derive(Clone)]
pub struct PathSegment {
    pub name: String,
    pub span: Span,
    // Lifetime args first (Rust convention), then type args. Either may be empty.
    pub lifetime_args: Vec<Lifetime>,
    pub args: Vec<Type>,
}

#[derive(Clone)]
pub struct MatchExpr {
    pub scrutinee: Box<Expr>,
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

#[derive(Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    // Optional `if guard` — reserved AST slot, parser accepts it; the
    // current typeck/codegen reject guards explicitly so we don't ship
    // half-implemented behavior. Wire up in a follow-up.
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone)]
pub struct IfLetExpr {
    pub pattern: Pattern,
    pub scrutinee: Box<Expr>,
    pub then_block: Box<Block>,
    pub else_block: Box<Block>,
}

#[derive(Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
    // Per-pattern NodeId, allocated by the parser on the same per-fn
    // counter as Expr.id. Typeck records each pattern's resolved type
    // here so codegen can read flatten layouts directly.
    pub id: NodeId,
}

#[derive(Clone)]
pub enum PatternKind {
    // `_` — matches anything, binds nothing.
    Wildcard,
    // `42`, `true`, `false`. Negative literals come through as a
    // separate `Neg(IntLit)` form when we add unary minus; for now
    // only non-negative ints + bools.
    LitInt(u64),
    LitBool(bool),
    // Identifier binding: `x`, `mut x`, `ref x`, `ref mut x`. A bare
    // `name` binds the matched value by-value (`mutable: false`,
    // `by_ref: false`); `mut name` binds by-value and lets the body
    // assign through it; `ref name` binds a *reference* to the matched
    // place (so the original value is not moved); `ref mut name` is
    // the unique-borrow form. Variant references must use a path
    // (`E::A`); a bare ident is always a binding.
    Binding {
        name: String,
        name_span: Span,
        by_ref: bool,
        mutable: bool,
    },
    // `Some(p1, p2)` / `Foo::Bar(p)` — variant or tuple-struct with
    // a positional payload.
    VariantTuple { path: Path, elems: Vec<Pattern> },
    // `Foo { a, b: pat, .. }` — struct-shaped variant or struct.
    // Each entry is `(field_name, pattern, span)`. Shorthand `a`
    // resolves to `(a, Pattern::Ident("a"), span)`. `rest` is true
    // if `..` was present (other fields ignored).
    VariantStruct {
        path: Path,
        fields: Vec<FieldPattern>,
        rest: bool,
    },
    // `(p, q, r)` — tuple pattern. `()` is the unit pattern; `(p,)`
    // is 1-element. Same trailing-comma rule as tuple expressions.
    Tuple(Vec<Pattern>),
    // `&pat` / `&mut pat` — match through a reference. The inner
    // pattern matches the pointee.
    Ref { inner: Box<Pattern>, mutable: bool },
    // `p1 | p2 | ...` — or-pattern. All alternatives must bind the
    // same set of names with compatible types. Nests anywhere a
    // pattern can.
    Or(Vec<Pattern>),
    // `lo..=hi` — inclusive range over an integer type. Both
    // endpoints are literals; type comes from context.
    Range { lo: u64, hi: u64 },
    // `name @ pat` — bind the matched value to `name` while also
    // matching `pat`. `pat` runs against the same value `name`
    // takes; bindings inside `pat` also enter scope.
    At { name: String, name_span: Span, inner: Box<Pattern> },
}

#[derive(Clone)]
pub struct FieldPattern {
    pub name: String,
    pub name_span: Span,
    pub pattern: Pattern,
}
