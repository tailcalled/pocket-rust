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
}

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
