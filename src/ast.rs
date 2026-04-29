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
}

#[derive(Clone)]
pub struct ImplBlock {
    pub type_params: Vec<TypeParam>,
    pub target: Path,
    pub methods: Vec<Function>,
    pub span: Span,
}

#[derive(Clone)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub type_params: Vec<TypeParam>,
    pub fields: Vec<StructField>,
}

#[derive(Clone)]
pub struct StructField {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
}

#[derive(Clone)]
pub struct Function {
    pub name: String,
    pub name_span: Span,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Block,
    // Number of NodeIds allocated within this function's body. Side vectors
    // (typeck/borrowck/codegen) sized to this length, indexed by Expr.id.
    pub node_count: u32,
}

#[derive(Clone)]
pub struct TypeParam {
    pub name: String,
    pub name_span: Span,
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
    Ref { inner: Box<Type>, mutable: bool },
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
    pub args: Vec<Type>,
}
