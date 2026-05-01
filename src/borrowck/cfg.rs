// CFG-based control flow representation for borrow checking.
//
// A `Cfg` is built from each function's AST body. The CFG is a graph of
// basic blocks; each block holds a sequence of `CfgStmt`s and ends in a
// `Terminator` that names successor block(s). Dataflow analyses
// (move/init, liveness, borrow regions) operate on this representation.
//
// The CFG is granular: every read, write, move, and borrow becomes an
// explicit operation. Compound expressions are lowered to a sequence of
// simple statements with temporary `Local`s for intermediate values, so
// that each step's effect on per-place state is unambiguous.

use crate::ast;
use crate::span::Span;
use crate::typeck::{LifetimeRepr, RType};

pub type BlockId = u32;
pub type LocalId = u32;

// A place — a memory location addressable by the program. A `Local`
// (parameter, let-binding, pattern-binding, or temporary) plus a
// sequence of projections that walk into its structure.
#[derive(Clone, PartialEq, Eq)]
pub struct Place {
    pub root: LocalId,
    pub projections: Vec<Projection>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Projection {
    Field(String),
    TupleIndex(u32),
    Deref,
}

impl Place {
    // True if `self` is a prefix of `other` (or equal). e.g.,
    // `[x]` is a prefix of `[x, .field, .0]`. Used by move
    // tracking: moving `x` invalidates uses of `x.field`.
    pub fn is_prefix_of(&self, other: &Place) -> bool {
        if self.root != other.root {
            return false;
        }
        if self.projections.len() > other.projections.len() {
            return false;
        }
        let mut i = 0;
        while i < self.projections.len() {
            if self.projections[i] != other.projections[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    // True if either place is a prefix of the other (overlapping).
    pub fn overlaps(&self, other: &Place) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }

    // Render a place as `name.field.0.field2`-style for error messages.
    // Compiler-introduced temporaries (no name) render as `<temp>` so the
    // message still scans even though the actual offender is a synthesized
    // local; source-level locals always have a name set by lowering.
    pub fn render(&self, locals: &Vec<LocalDecl>) -> String {
        let mut out = match &locals[self.root as usize].name {
            Some(n) => n.clone(),
            None => "<temp>".to_string(),
        };
        let mut i = 0;
        while i < self.projections.len() {
            match &self.projections[i] {
                Projection::Field(f) => {
                    out.push('.');
                    out.push_str(f);
                }
                Projection::TupleIndex(n) => {
                    out.push('.');
                    out.push_str(&n.to_string());
                }
                Projection::Deref => {
                    out = format!("(*{})", out);
                }
            }
            i += 1;
        }
        out
    }
}

// An operand: either a copy of a place's value (Copy types) or a move
// (consuming). The choice is made at lowering time using the typeck
// `is_copy` query, so analyses don't need to re-check. The `span`
// pinpoints the source location of the use itself — analyses use it
// for error attribution (e.g. "value moved here") so a Call with
// several operand arguments can blame the offending arg precisely
// rather than the surrounding statement. The `node_id` is the AST
// `Expr.id` that produced this operand (when it came from an
// expression in source); used by drop-flag synthesis to map move
// sites back to the original AST node so codegen can clear the flag
// at the matching codegen point.
#[derive(Clone)]
pub struct Operand {
    pub kind: OperandKind,
    pub span: Span,
    pub node_id: Option<crate::ast::NodeId>,
}

#[derive(Clone)]
pub enum OperandKind {
    Move(Place),
    Copy(Place),
    // Constants don't borrow or move; they materialize a value out of
    // thin air. Carried inline because they're tiny.
    ConstInt(u64),
    ConstBool(bool),
    ConstUnit,
}

// How to compute a value to assign to a place.
#[derive(Clone)]
pub enum Rvalue {
    Use(Operand),
    Borrow {
        mutable: bool,
        place: Place,
        // Lifetime label used by NLL borrow-region computation. Each
        // borrow expression at AST→CFG time gets a fresh region id; the
        // NLL pass computes the set of CFG points the region must
        // include based on uses.
        region: RegionId,
    },
    Cast {
        source: Operand,
        target_ty: RType,
    },
    // Calls — covers free functions, methods, and variant
    // constructors. Resolution to a callable happens at typeck (see
    // `MethodResolution`/`CallResolution`); the CFG carries the
    // resolved-callee `node_id` so codegen-side lookup re-uses the
    // typeck record.
    Call {
        callee: CallTarget,
        args: Vec<Operand>,
        // The `Expr.id` of the originating call expression — gives
        // analyses access to typeck's resolution and lifetime metadata.
        call_node_id: ast::NodeId,
    },
    StructLit {
        type_path: Vec<String>,
        type_args: Vec<RType>,
        fields: Vec<(String, Operand)>,
    },
    Tuple(Vec<Operand>),
    Variant {
        enum_path: Vec<String>,
        type_args: Vec<RType>,
        disc: u32,
        fields: VariantFields,
    },
    Builtin {
        name: String,
        args: Vec<Operand>,
    },
    // Read the discriminant of an enum-typed place. The result is an
    // i32 holding the variant index, used by match's `SwitchInt`
    // terminator. The place itself isn't moved — analyses treat this
    // as a non-consuming read.
    Discriminant(Place),
}

#[derive(Clone)]
pub enum VariantFields {
    Unit,
    Tuple(Vec<Operand>),
    Struct(Vec<(String, Operand)>),
}

#[derive(Clone)]
pub enum CallTarget {
    // Fully resolved callee path; analyses hand this to typeck for
    // signature lookup.
    Path(Vec<String>),
    // Method dispatch deferred to typeck records keyed by node id; the
    // CFG just carries the marker.
    MethodResolution(ast::NodeId),
}

pub type RegionId = u32;

// A statement — a single operation that doesn't affect control flow.
// Each statement carries the source span of the originating AST node so
// diagnostics can point at the right place.
pub struct CfgStmt {
    pub kind: CfgStmtKind,
    pub span: Span,
}

pub enum CfgStmtKind {
    // `place = rvalue` — covers the let-init, assignment, and
    // intermediate temp materialization cases. The place must be
    // initialized after this statement.
    Assign { place: Place, rvalue: Rvalue },
    // `drop(place)` — explicit destructor call inserted at scope end
    // for Drop-typed locals.
    Drop(Place),
    // `StorageLive(local)` — the local enters scope; its address-taken
    // backing storage becomes valid. Inserted at the let-stmt or at
    // function entry for parameters.
    StorageLive(LocalId),
    // `StorageDead(local)` — the local exits scope; its storage may be
    // reused. Inserted at the end of the local's containing block.
    StorageDead(LocalId),
}

pub enum Terminator {
    // Unconditional jump to a successor block.
    Goto(BlockId),
    // Branch on a bool operand: true → then_block, false → else_block.
    If {
        cond: Operand,
        then_block: BlockId,
        else_block: BlockId,
    },
    // Branch on an integer operand (used by match discriminant tests).
    // Each `(value, BlockId)` pair is a target; `otherwise` is the
    // fallback when no value matches.
    SwitchInt {
        operand: Operand,
        targets: Vec<(u64, BlockId)>,
        otherwise: BlockId,
    },
    // Function exit. The function's return value (if any) is in
    // `Local(0)` by convention.
    Return,
    // Unreachable — produced by exhaustive matches' fall-through guard
    // and by builtins that always trap (e.g., 128-bit mul).
    Unreachable,
}

pub struct BasicBlock {
    pub stmts: Vec<CfgStmt>,
    pub terminator: Terminator,
}

// Per-local metadata: name (for diagnostics), type, source span (the
// let-stmt or param decl), and whether it was declared `mut`.
pub struct LocalDecl {
    pub name: Option<String>,
    pub ty: RType,
    pub span: Span,
    pub mutable: bool,
    // True for compiler-introduced temporaries (intermediate values from
    // expression lowering); false for source-level bindings.
    pub is_temp: bool,
}

pub struct Cfg {
    pub blocks: Vec<BasicBlock>,
    pub locals: Vec<LocalDecl>,
    pub entry: BlockId,
    // The number of regions allocated during construction. Each Borrow
    // rvalue carries a region id < `region_count`.
    pub region_count: u32,
    // Local 0 holds the function's return value when the return type is
    // non-unit. Parameters occupy locals 1..=param_count.
    pub return_local: Option<LocalId>,
    pub param_count: u32,
}
