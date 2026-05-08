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
    TypeAlias(TypeAlias),
    Const(ConstDecl),
}

// `pub? const NAME: TYPE = EXPR;` — a named compile-time constant.
// MVP: `EXPR` must be evaluable to a primitive literal during typeck
// setup. Path references that resolve to a const become Use sites
// whose codegen-time emission inlines the value. No `const fn`
// — the value computation is purely literal.
#[derive(Clone)]
pub struct ConstDecl {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
    pub value: Expr,
    pub is_pub: bool,
    pub span: Span,
}

// `pub? type Name<'a, T> = TypeExpr;` — a name for an existing type.
// Generic params on the lhs scope into the target; the alias is fully
// transparent (typeck resolves uses by substituting the target type
// with the call-site's type-args, then continuing as if the user had
// written the target). No new type is introduced — `type Foo = u32`
// makes `Foo` and `u32` interchangeable, not nominally distinct.
#[derive(Clone)]
pub struct TypeAlias {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub target: Type,
    pub is_pub: bool,
    pub span: Span,
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
    // `type Name = Type;` bindings inside the impl body. Trait impls
    // must provide one binding per associated type the trait declares;
    // inherent impls are not allowed to declare any.
    pub assoc_type_bindings: Vec<ImplAssocType>,
    // `where` clause on the impl block. Predicates with a bare-type-param
    // LHS are merged into the matching type-param's bounds at setup
    // time; complex-LHS predicates are stored separately and enforced
    // at impl resolution.
    pub where_clause: Vec<WherePredicate>,
    pub span: Span,
}

// `type Name = ConcreteType;` inside an impl body.
#[derive(Clone)]
pub struct ImplAssocType {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
}

#[derive(Clone)]
pub struct TraitDef {
    pub name: String,
    pub name_span: Span,
    // Trait-level type parameters (`trait Add<Rhs> { ... }`). Available
    // inside the trait body's method signatures and assoc-type defaults.
    // Each bound site that names this trait supplies values for these.
    pub type_params: Vec<TypeParam>,
    pub supertraits: Vec<TraitBound>,
    pub methods: Vec<TraitMethodSig>,
    // `type Name;` declarations inside the trait body. Each impl of
    // this trait must bind every name listed here.
    pub assoc_types: Vec<TraitAssocType>,
    pub span: Span,
    pub is_pub: bool,
}

// `type Name;` inside a trait body. No defaults, no bounds yet.
#[derive(Clone)]
pub struct TraitAssocType {
    pub name: String,
    pub name_span: Span,
}

#[derive(Clone)]
pub struct TraitMethodSig {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub where_clause: Vec<WherePredicate>,
}

#[derive(Clone)]
pub struct TraitBound {
    pub path: Path,
    // `Trait<Name = Type>` constraints. Each entry pins one of the
    // trait's associated types to a specific type at the bound site.
    // Empty for plain `Trait` bounds.
    pub assoc_constraints: Vec<AssocConstraint>,
    // `for<'a, 'b>` HRTB lifetime params declared at the bound. Empty
    // for ordinary bounds. The lifetimes are in scope inside this
    // bound's path/args/assoc-constraint types only — they do not
    // leak out to the enclosing fn/impl's lifetime scope.
    pub hrtb_lifetime_params: Vec<LifetimeParam>,
}

// One predicate of a `where` clause. Two shapes:
//   * `Type` — `<type>: <Trait1> + <Trait2> + 'lt`. The `bounds`
//     hold the trait obligations; `lifetime_bounds` hold the
//     trailing `+ 'lifetime` outlives obligations on the type.
//   * `Lifetime` — `'a: 'b + 'c`. An outlives obligation on
//     lifetimes alone. Real Rust uses these to declare relations
//     the borrow checker can rely on; pocket-rust's lifetime
//     checking is Phase B structural-only, so these are validated
//     for in-scope lifetimes at setup but don't yet constrain
//     borrowck.
#[derive(Clone)]
pub enum WherePredicate {
    Type {
        lhs: Type,
        bounds: Vec<TraitBound>,
        lifetime_bounds: Vec<Lifetime>,
        span: Span,
    },
    Lifetime {
        lhs: Lifetime,
        bounds: Vec<Lifetime>,
        span: Span,
    },
}

impl WherePredicate {
    pub fn span(&self) -> &Span {
        match self {
            WherePredicate::Type { span, .. } => span,
            WherePredicate::Lifetime { span, .. } => span,
        }
    }
}

// `Name = Type` inside a `Trait<…>` bound.
#[derive(Clone)]
pub struct AssocConstraint {
    pub name: String,
    pub name_span: Span,
    pub ty: Type,
}

#[derive(Clone)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub lifetime_params: Vec<LifetimeParam>,
    pub type_params: Vec<TypeParam>,
    pub fields: Vec<StructField>,
    pub is_pub: bool,
    // `#[derive(Trait1, Trait2)]` clauses captured at parse time. The
    // separate `derive_expand` stage consumes them and synthesizes the
    // corresponding trait impls — typeck never sees this field.
    pub derives: Vec<DeriveClause>,
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
    pub derives: Vec<DeriveClause>,
}

// Captured `#[derive(Trait1, Trait2, ...)]` attribute. One clause per
// `#[derive(...)]` literal — multiple attributes flatten into multiple
// clauses on the same def.
#[derive(Clone)]
pub struct DeriveClause {
    pub traits: Vec<DeriveTrait>,
    pub attr_span: Span,
}

#[derive(Clone)]
pub struct DeriveTrait {
    pub name: String,
    pub name_span: Span,
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
    // `where` clause: trailing list of predicates after `-> R`, before
    // the body brace. Predicates with a bare-type-param LHS merge into
    // the matching type-param's inline bounds at setup time; complex-
    // LHS predicates are stored on the FnSymbol/GenericTemplate and
    // enforced at call sites after substitution.
    pub where_clause: Vec<WherePredicate>,
    // Number of NodeIds allocated within this function's body. Side vectors
    // (typeck/borrowck/codegen) sized to this length, indexed by Expr.id.
    pub node_count: u32,
    pub is_pub: bool,
    // `unsafe fn …` — calls to this function must lexically appear
    // inside an `unsafe { … }` block, and the function's body is
    // implicitly in unsafe context (so it can deref raw pointers and
    // call other unsafe functions without an inner block).
    pub is_unsafe: bool,
}

#[derive(Clone)]
pub struct TypeParam {
    pub name: String,
    pub name_span: Span,
    // Trait bounds attached to this type param (e.g. `<T: Show + Eq>`).
    // Empty when no bounds were written.
    pub bounds: Vec<TraitBound>,
    // Default type for this param (e.g. `Rhs = Self` in `trait Add<Rhs
    // = Self>`). Currently only meaningful on trait declarations —
    // function/struct/impl type-params parse `default` but it's
    // semantically rejected at typeck setup. `Self` in a default
    // refers to the implementing type at use sites (substituted by
    // the trait-arg defaulter).
    pub default: Option<Type>,
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
    // `[T]` — the dynamically-sized slice type. Only valid behind a
    // reference (`&[T]` / `&mut [T]`). Bare `[T]` in a position where
    // a sized type is required is rejected at type-resolution time.
    Slice(Box<Type>),
    // `!` — the never type. Has no inhabitants; produced by `break`,
    // `continue`, `return`, and calls to functions returning `!`.
    // Coerces freely to any other type at unification time so that an
    // arm of an `if`/`match` can `break` while the other arm yields a
    // real value, with the construct typed as the real value's type.
    Never,
    // (`str` is a single-segment Path("str") at the AST level — the
    // type-resolver maps it to `RType::Str`. No dedicated AST variant.)

    // `impl T1 + T2` (argument-position impl trait). The parser
    // recognizes this anywhere `parse_type` is called, but only the
    // top of a fn parameter is a valid position: after parsing fn
    // params, `parse_function_with_vis` walks each param.ty and
    // desugars top-level `ImplTrait` into a fresh anonymous type-param
    // (`__impl_<n>`) with the recorded bounds. Any `ImplTrait` that
    // survives into typeck (return position, struct field, alias, …)
    // is rejected with "`impl Trait` not allowed here".
    ImplTrait(Vec<TraitBound>),
    // `_` in type position — a placeholder for type inference. Valid
    // in turbofish args and let-annotations (`let x: Vec<_> = …`,
    // `Vec::<_>::new()`). At those sites typeck pre-walks the AST,
    // replacing each Placeholder with a synth `Param` name, calls
    // `resolve_type` with the synth name added to `type_params`, and
    // substitutes each `Param(synth)` for a fresh inference var. In
    // any other position (fn return, struct field, alias, …) the
    // standard `resolve_type` rejects it with a clear error.
    Placeholder,
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

// Helper for downstream passes that want fast access to the
// "simple `let x` / `let mut x`" common case without walking the
// pattern. Returns `Some((name, mutable, name_span))` when the
// pattern is a bare `Binding` (no `ref`, no inner pattern); `None`
// for wildcards, tuple destructure, etc.
pub fn let_simple_binding<'a>(ls: &'a LetStmt) -> Option<(&'a str, bool, &'a Span)> {
    if let PatternKind::Binding { name, name_span, by_ref: false, mutable } = &ls.pattern.kind {
        Some((name.as_str(), *mutable, name_span))
    } else {
        None
    }
}

#[derive(Clone)]
pub struct LetStmt {
    // Patterns in let: bare-ident `let x = e;` is `Pattern::Binding`,
    // `let _ = e;` is `Pattern::Wildcard`, `let (a, b) = e;` is
    // `Pattern::Tuple`. Refutable patterns (variant constructors,
    // literals, ranges) are only allowed when `else_block` is `Some`
    // (i.e. let-else).
    pub pattern: Pattern,
    pub ty: Option<Type>,
    // `None` for declared-but-uninitialized `let x: T;`. Typeck
    // requires `ty: Some(_)` and a single `Binding` pattern (no
    // destructure / wildcard / refutable / let-else) when `value` is
    // `None`. Borrowck seeds the binding's place into the move-state
    // lattice as `Moved`, so any read before the first assignment is
    // rejected with an "uninitialized" diagnostic; an assignment
    // returns it to `Init`. Codegen emits no initializer — the stack
    // slot is allocated by frame layout and left uninitialized.
    pub value: Option<Expr>,
    // `let PAT = EXPR else { … };` — when `Some`, the pattern may be
    // refutable; if it doesn't match `EXPR`, the else block runs.
    // The else block must diverge (`return`, `break`, `continue`,
    // `panic!()`, …) so control flow can't fall through past a
    // mismatched binding. Mutually exclusive with `value: None`
    // (an uninit `let` cannot have an else block — there's nothing
    // to test).
    pub else_block: Option<Box<Block>>,
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
    // `-INT_LIT` parsed as a single negative literal — e.g. `-4` is
    // `NegIntLit(4)`. The value stored is the **absolute magnitude**;
    // typeck pins the literal's type to a signed integer and codegen
    // lowers as `from_i64(-(value as i64))`. For non-literal unary
    // minus (`-x`, `-(a + b)`, etc.) the parser desugars to a
    // `<T as VecSpace>::neg` method call instead.
    NegIntLit(u64),
    // `"..."` — UTF-8 string literal. The decoded payload is stored
    // directly on the AST node; codegen interns it into the module's
    // data section and emits a fat ref pointing into that section.
    // Type is always `&'static str`.
    StrLit(String),
    BoolLit(bool),
    // `'X'` / `'\n'` / `'¥'` — char literal. Carries the Unicode
    // codepoint as a u32. Type at use site is `char` (no integer-
    // literal-style inference), but `as` casts let user code convert
    // to/from integer types.
    CharLit(u32),
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
    // `¤name(args)` or `¤name::<TypeArgs...>(args)` — a compiler-
    // builtin intrinsic call. The name identifies which primitive op
    // (e.g. `u32_add`, `i64_eq`, `bool_and`, `alloc`, `free`, `cast`).
    // Type-parameterized builtins (currently only `cast`) require an
    // explicit turbofish; type inference is not used. Typeck validates
    // the name + arg shape; codegen lowers to a small sequence of wasm
    // instructions.
    Builtin {
        name: String,
        name_span: Span,
        type_args: Vec<Type>,
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
    // `'label: while cond { body }` or `while cond { body }`. The
    // expression's type is `()` regardless of body. The optional label
    // can be targeted by `break 'label` / `continue 'label` from
    // nested loops.
    While(WhileExpr),
    // `'label: for pat in iter { body }` / `for pat in iter { body }`.
    // See `ForLoop` for the iter-trait expectation; the loop yields
    // `()` regardless of body shape.
    For(ForLoop),
    // `break;` / `break 'label;`. Type `!` (diverging). The optional
    // label names which enclosing loop to exit.
    Break {
        label: Option<String>,
        label_span: Option<Span>,
    },
    // `continue;` / `continue 'label;`. Type `!` (diverging). Skips
    // the rest of the loop body and re-enters the named loop.
    Continue {
        label: Option<String>,
        label_span: Option<Span>,
    },
    // `return;` (unit) or `return EXPR;`. Type `!` (diverging). The
    // value's type unifies against the enclosing function's return
    // type at typeck. Codegen drops in-scope bindings, restores SP,
    // and emits the wasm `return` (with sret memcpy first, when the
    // function uses the sret ABI).
    Return {
        value: Option<Box<Expr>>,
    },
    // `expr?` — try-operator postfix. `expr` must be `Result<T, E>`
    // and the enclosing function must return `Result<U, E>` (matching
    // E). On `Err(e)` the function returns `Err(e)` immediately; on
    // `Ok(v)` the expression evaluates to `v`. Kept as a first-class
    // node (not desugared to match+return early) so error spans point
    // at the `?` site rather than at synthetic match arms.
    Try {
        inner: Box<Expr>,
        // Span of the `?` token itself, used for diagnostics.
        question_span: Span,
    },
    // `arr[idx]` — indexing. Always typechecks as a read via the
    // `Index` trait (returns the trait's `Output` type). Codegen
    // branches on the *enclosing* context:
    //   - value position → emit `*arr.index(idx)`.
    //   - `&arr[idx]` → `arr.index(idx)`.
    //   - `&mut arr[idx]` / LHS of `=` → `arr.index_mut(idx)`.
    // Kept as a first-class node (not desugared early) so error
    // diagnostics point at the `[` / `]` and the typeck failure
    // surfaces as "no `Index` impl for X".
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        // Span of the `[idx]` brackets — used for diagnostics.
        bracket_span: Span,
    },
    // `name!(args)` — macro invocation. Currently only `panic!(msg)`
    // is recognized; typeck rejects other names. Kept as a generic
    // `MacroCall` so future macros (e.g. `assert!`, `println!`) can
    // hang off the same node.
    MacroCall {
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    // `|args| body` or `move |args| body` — closure expression. Each
    // closure has an anonymous nominal type (synthesized at typeck) and
    // implements one of `Fn` / `FnMut` / `FnOnce` based on what its body
    // does to its captures. Args may carry optional explicit type
    // annotations; missing annotations are inferred from the call-site
    // bound's `Fn(Args)` tuple. The body extends as far right as the
    // surrounding expression context allows (lowest precedence —
    // `|x| x + 1` is `|x| (x + 1)`). Detailed semantics live in the
    // `closures-and-fn-traits` skill.
    Closure(Closure),
}

#[derive(Clone)]
pub struct Closure {
    pub params: Vec<ClosureParam>,
    // Optional `-> T` annotation on the body's return type. None means
    // the body's natural type (inferred).
    pub return_type: Option<Type>,
    pub body: Box<Expr>,
    // `move |x| ...` — force every capture to be by-value (owned/Copy)
    // regardless of how the body uses it. Without `move`, capture mode
    // per binding is inferred from body usage.
    pub is_move: bool,
    // Whole-closure span: from `|` (or `move`) through end of body.
    pub span: Span,
}

#[derive(Clone)]
pub struct ClosureParam {
    // Irrefutable pattern. The common case is a single `Binding`
    // (i.e. an identifier), but tuple destructure (`|(a, b)|`),
    // wildcards (`|_|`), and other irrefutable patterns are accepted.
    // Refutability is checked at typeck time against the param's
    // inferred type.
    pub pattern: Pattern,
    // None when the param's type is left to inference. When `Some`, used
    // as the param's declared type and also unified against the
    // surrounding `Fn(...)` bound's slot.
    pub ty: Option<Type>,
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
pub struct WhileExpr {
    pub label: Option<String>,
    pub label_span: Option<Span>,
    pub cond: Box<Expr>,
    pub body: Box<Block>,
}

// `'label: for pat in iter { body }` / `for pat in iter { body }`.
// Stays as a first-class node through typeck (so error messages can
// keep mentioning `for`), borrowck (lowered to a CFG that mirrors
// `loop { match Iterator::next(&mut __iter) { Some(pat) => body,
// None => break } }`), and codegen (same shape in wasm). The
// `iter`'s type must implement `std::iter::Iterator` directly —
// pocket-rust doesn't yet auto-call `IntoIterator::into_iter`, so
// users write `vec.into_iter()` explicitly.
#[derive(Clone)]
pub struct ForLoop {
    pub label: Option<String>,
    pub label_span: Option<Span>,
    pub pattern: Pattern,
    pub iter: Box<Expr>,
    pub body: Box<Block>,
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
