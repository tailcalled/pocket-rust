use crate::span::Span;

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
}

pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<StructField>,
}

pub struct StructField {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
}

pub struct Function {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Block,
}

pub struct Param {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
}

pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

pub enum TypeKind {
    Usize,
    Struct(Path),
    Ref(Box<Type>),
}

pub struct Block {
    pub tail: Option<Expr>,
    pub span: Span,
}

pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

pub enum ExprKind {
    UsizeLit(u64),
    Call(Call),
    Var(String),
    StructLit(StructLit),
    FieldAccess(FieldAccess),
    Borrow(Box<Expr>),
}

pub struct Call {
    pub callee: Path,
    pub args: Vec<Expr>,
}

pub struct StructLit {
    pub path: Path,
    pub fields: Vec<FieldInit>,
}

pub struct FieldInit {
    pub name: String,
    pub name_span: Span,
    pub value: Expr,
}

pub struct FieldAccess {
    pub base: Box<Expr>,
    pub field: String,
    pub field_span: Span,
}

pub struct Path {
    pub segments: Vec<PathSegment>,
    pub span: Span,
}

pub struct PathSegment {
    pub name: String,
    pub span: Span,
}
