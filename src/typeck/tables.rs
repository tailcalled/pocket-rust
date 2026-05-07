use super::{LifetimeRepr, RType};
use crate::span::Span;

// Per-closure information populated by typeck and consumed by the
// post-typeck lowering pass. Indexed on `FnSymbol.closures` /
// `GenericTemplate.closures` by the closure expression's `Expr.id`
// — `Some(_)` only at closure expression sites; `None` elsewhere.
//
// The lowering pass reads `synthesized_struct_path` to know the unit
// struct it should attach impl(s) to, then emits `impl Fn<(..)> for
// __closure_<id>` (and FnMut/FnOnce mirrors when capture analysis
// allows them) using `param_types` / `return_type`. The body the
// lowering moves into the `call`/`call_mut`/`call_once` method is the
// original `Closure.body` Expr — typeck has already validated it.
//
// `captures` is empty in phase 1 (non-capturing). When phase 2 lands,
// it carries one `CaptureInfo` per outer binding referenced from the
// body — used by lowering to add struct fields and rewrite
// `Var(captured_name)` → `(*)?self.<binding_name>` inside the impl
// method body.
#[derive(Clone)]
pub struct ClosureInfo {
    // Path of the synthesized unit struct that represents this closure's
    // type. Allocated by typeck (`__closure_<crate_counter>`) and fed
    // through to the lowering pass which generates the matching
    // `Item::Struct` and `Item::Impl` nodes.
    pub synthesized_struct_path: Vec<String>,
    pub param_types: Vec<RType>,
    pub return_type: RType,
    pub is_move: bool,
    // Empty for phase 1 (non-capturing). Phase 2 fills in one entry per
    // binding the body references from outside the closure's param
    // scope, with the resolved `captured_ty` and inferred `mode`.
    pub captures: Vec<CaptureInfo>,
    pub body_span: Span,
    // Source file containing the closure expression. Used by post-typeck
    // struct registration to populate StructEntry.file, and by the
    // lowering pass to attach the synthesized impl to the right module.
    pub source_file: String,
    // True when the body mutates any captured binding (assignment,
    // compound-assign, `&mut`-borrow). Lowering uses this to skip the
    // `Fn` impl (which would dispatch via `&self` and so couldn't
    // mutate captured values) and synthesize only `FnMut` + `FnOnce`.
    pub body_mutates_capture: bool,
    // Type-parameters of the enclosing function, copied at closure-
    // typeck time. The synthesized struct + impl carry these so that
    // a closure inside `fn helper<T>(x: T)` synthesizes `struct
    // __closure_<id><T>` and `impl<T> Fn<(T,)> for __closure_<id><T>`,
    // letting `T` references in the closure body resolve when the
    // synthesized method is re-typed. Empty for closures inside
    // non-generic functions.
    pub enclosing_type_params: Vec<String>,
}

#[derive(Clone)]
pub struct CaptureInfo {
    pub binding_name: String,
    // Type stored on the synthesized struct's field — `T` for owned/Copy
    // captures, `&T` for shared borrows, `&mut T` for unique borrows.
    pub captured_ty: RType,
    pub mode: CaptureMode,
}

#[derive(Clone, Copy)]
pub enum CaptureMode {
    Ref,
    RefMut,
    Move,
}

#[derive(Clone)]
pub struct RTypedField {
    pub name: String,
    pub name_span: Span,
    pub ty: RType,
    pub is_pub: bool,
}

pub struct StructEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub type_params: Vec<String>,
    // Lifetime params declared on the struct (e.g., `struct Holder<'a, T>`
    // gives `lifetime_params = ["a"]`). Empty for non-lifetime-generic
    // structs. Used to validate lifetime args at type-position uses and to
    // build a substitution env when reading field types.
    pub lifetime_params: Vec<String>,
    pub fields: Vec<RTypedField>,
    pub is_pub: bool,
}

pub struct StructTable {
    pub entries: Vec<StructEntry>,
}

pub fn struct_lookup<'a>(table: &'a StructTable, path: &Vec<String>) -> Option<&'a StructEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if &table.entries[i].path == path {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

// Enum table — analogous to StructTable. Each entry records the enum's
// variants with their resolved payload types. Generic enums carry their
// type/lifetime param names; layout (`byte_size_of` etc.) substitutes
// type_args at use-site to compute concrete sizes.
pub struct EnumEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub type_params: Vec<String>,
    pub lifetime_params: Vec<String>,
    pub variants: Vec<EnumVariantEntry>,
    pub is_pub: bool,
}

pub struct EnumVariantEntry {
    pub name: String,
    pub name_span: Span,
    // 0-based discriminant in declaration order. Stored as u32 (we
    // emit it as i32.const at codegen).
    pub disc: u32,
    pub payload: VariantPayloadResolved,
}

pub enum VariantPayloadResolved {
    Unit,
    Tuple(Vec<RType>),
    Struct(Vec<RTypedField>),
}

pub struct EnumTable {
    pub entries: Vec<EnumEntry>,
}

pub fn enum_lookup<'a>(table: &'a EnumTable, path: &Vec<String>) -> Option<&'a EnumEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if &table.entries[i].path == path {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

// Per-place move state recorded by borrowck. `Moved` means moved on
// every reachable path; `MaybeMoved` means moved on some paths but not
// others (the binding's storage is potentially-init at the place's
// scope-end, requiring a runtime drop flag in codegen). The implicit
// third state — `Init` — is "the place isn't in the list at all."
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MoveStatus {
    Moved,
    MaybeMoved,
}

// Per-pattern.id record of "match ergonomics" decisions made by typeck.
// Default-zero means "no ergonomics applied at this pattern node — mono
// can lower the AST as written". Mono reads this side table to rebuild
// the pattern with explicit `&` wrappers and `ref` bindings before
// codegen sees it.
//
// Real Rust's match-ergonomics RFC 2005: when a non-reference pattern
// matches a `&T`/`&mut T` scrutinee, the reference layer is auto-peeled
// and the "default binding mode" inside that pattern flips from Move to
// Ref/RefMut. Pocket-rust mirrors this — typeck records *what* it did
// here so mono can reify the desugar without re-derive anything.
#[derive(Clone, Copy)]
pub struct PatternErgo {
    // Number of `&` layers typeck auto-peeled before applying this
    // pattern. Zero means "pattern matched the scrutinee shape natively"
    // (the common case). Each peel layer's mutability is encoded in
    // `peel_mut_bits` (bit 0 = outermost peel).
    pub peel_layers: u8,
    pub peel_mut_bits: u8,
    // For Binding/At patterns: when true, the binding's effective mode
    // is `&T` or `&mut T` (overriding the AST's `by_ref: false` written
    // form). False means "use AST as written". `binding_mutable_ref`
    // selects between `&T` (false) and `&mut T` (true) when the override
    // is active.
    pub binding_override_ref: bool,
    pub binding_mutable_ref: bool,
}

impl Default for PatternErgo {
    fn default() -> Self {
        PatternErgo {
            peel_layers: 0,
            peel_mut_bits: 0,
            binding_override_ref: false,
            binding_mutable_ref: false,
        }
    }
}

#[derive(Clone)]
pub struct MovedPlace {
    pub place: Vec<String>,
    pub status: MoveStatus,
}

// Type-alias declarations (`pub? type Name<...>? = TypeExpr;`).
// Registered during typeck setup before the rest of the type pipeline
// runs, so `resolve_type` can substitute alias references with their
// (already-resolved) target. Aliases are fully transparent — they
// don't introduce a nominal type. Generic aliases (with type-params)
// store the target with `RType::Param(...)` slots that the use site
// substitutes via the path's type-args.
pub struct AliasTable {
    pub entries: Vec<AliasEntry>,
}

pub struct AliasEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub type_params: Vec<String>,
    pub lifetime_params: Vec<String>,
    pub target: RType,
    pub is_pub: bool,
}

pub fn alias_lookup<'a>(table: &'a AliasTable, path: &Vec<String>) -> Option<&'a AliasEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if &table.entries[i].path == path {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

// Trait declarations registered during the first typeck pass. Trait
// methods' signatures are stored with `Self` as `RType::Param("Self")` so
// impl validation can substitute against the impl target.
pub struct TraitTable {
    pub entries: Vec<TraitEntry>,
    // Each `impl Trait for Target` row registered. Multiple rows for the
    // same `(trait_path, target_pattern)` are rejected as duplicates.
    pub impls: Vec<TraitImplEntry>,
}

// One supertrait edge declared on a trait. `args` reference the
// trait's own type-params (and `Self`); at the obligation check site,
// `args` are substituted using the impl's `trait_args` mapping plus
// `Self → impl_target`, then `solve_impl_with_args(path, args, target)`
// looks for the matching impl.
pub struct SupertraitRef {
    pub path: Vec<String>,
    pub args: Vec<RType>,
}

pub struct TraitEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub methods: Vec<TraitMethodEntry>,
    pub is_pub: bool,
    pub supertraits: Vec<SupertraitRef>,
    // `type Name;` declarations inside the trait body. Each impl of
    // this trait must bind exactly these names (no missing, no extras).
    pub assoc_types: Vec<String>,
    // Trait-level type parameter names (`trait Add<Rhs> { ... }`).
    // Each `impl Add<X> for T { ... }` row supplies one RType per name
    // here in declaration order; downstream dispatch reads
    // `TraitImplEntry.trait_args` to disambiguate which row applies.
    pub trait_type_params: Vec<String>,
    // Per trait-level type-param, the resolved default type (`Rhs =
    // Self` → `Some(Param("Self"))`); `None` if no default. Same length
    // and order as `trait_type_params`. Trait-arg lists shorter than
    // `trait_type_params.len()` are completed by appending defaults
    // (with `Self` substituted by the impl target / bound holder).
    // A param with no default is required at use sites.
    pub trait_type_param_defaults: Vec<Option<RType>>,
}

pub struct TraitMethodEntry {
    pub name: String,
    pub name_span: Span,
    // Method-level type-params declared on the trait method (e.g. `fn
    // bar<U>(self, u: U)`). Names appear in `param_types` / `return_type`
    // as `RType::Param(name)`. Validation against impl methods compares
    // by arity + α-equivalence (impl's `<V>` matched positionally with
    // trait's `<U>`); symbolic dispatch allocates fresh inference vars
    // per call, optionally pinned by turbofish.
    pub type_params: Vec<String>,
    // Per-type-param trait-bound paths, parallel to `type_params`.
    // Populated from the method's inline `<U: Bound>` declarations
    // and from where-clause Param-LHS predicates. Impls of this
    // trait inherit these bounds — so an impl method's body sees
    // `U: Bound` even if the impl signature doesn't restate it.
    pub type_param_bounds: Vec<Vec<Vec<String>>>,
    // Resolved param types in declaration order. Param 0 is the receiver
    // (when the method has one); `Self` appears as `RType::Param("Self")`
    // and gets substituted with the impl target during validation +
    // dispatch.
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    // Receiver shape if param 0 is a `self` receiver — Move (`self:
    // Self`), BorrowImm (`&Self`), or BorrowMut (`&mut Self`). None for
    // associated functions without a receiver.
    pub receiver_shape: Option<TraitReceiverShape>,
}

#[derive(Clone, Copy)]
pub enum TraitReceiverShape {
    Move,
    BorrowImm,
    BorrowMut,
}

// One `impl Trait for Target` row. `target` is the impl-target pattern
// (as in inherent impls — see `FnSymbol.impl_target`); `impl_type_params`
// records the impl's own type-params (not the trait's).
pub struct TraitImplEntry {
    pub trait_path: Vec<String>,
    // Resolved positional type-args for the trait (the `<U>` in
    // `impl Add<U> for T`). Length matches the trait's
    // `trait_type_params`. May contain `Param(name)` slots referring
    // to this impl's own type-params (e.g. `impl<X> Add<X> for X`).
    pub trait_args: Vec<RType>,
    pub target: RType,
    pub impl_type_params: Vec<String>,
    pub impl_lifetime_params: Vec<String>,
    // Per impl-type-param trait bounds (resolved). Same shape and order as
    // `impl_type_params`. `solve_impl` enforces these recursively when
    // matching a candidate impl against a concrete type.
    pub impl_type_param_bounds: Vec<Vec<Vec<String>>>,
    // Resolved associated-type bindings declared inside the impl body
    // (`type Name = T;`). One entry per name listed by the trait's
    // `assoc_types`, in the same order. Validated against the trait at
    // setup time.
    pub assoc_type_bindings: Vec<(String, RType)>,
    pub file: String,
    pub span: Span,
}
pub fn trait_lookup<'a>(table: &'a TraitTable, path: &Vec<String>) -> Option<&'a TraitEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if &table.entries[i].path == path {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}
pub struct FnSymbol {
    pub path: Vec<String>,
    pub idx: u32,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    // For trait-impl methods, the index into `TraitTable.impls` of the
    // owning impl row. None for free fns and inherent methods.
    pub trait_impl_idx: Option<usize>,
    pub is_pub: bool,
    // `unsafe fn` — call sites must be lexically inside an `unsafe { … }`
    // block (enforced by `safeck`); the body is implicitly in unsafe
    // context.
    pub is_unsafe: bool,
    // Per `Expr` node, indexed by `Expr.id`. Contains the resolved `RType`
    // for nodes that carry a value type. `None` for nodes without one
    // (currently unused — every Expr produces a value in our subset).
    // Borrowck reads this for binding types (via `let_stmt.value.id`),
    // codegen reads this for layout (let bindings, lit constants, struct
    // literals), safeck reads `Deref(inner).inner.id`'s entry to detect
    // raw-pointer derefs.
    pub expr_types: Vec<Option<RType>>,
    // Outermost lifetime of each param's ref type, or None for non-ref
    // params. Used by borrowck to map a returned ref's lifetime back to the
    // arg slot(s) whose borrows it inherits.
    pub param_lifetimes: Vec<Option<LifetimeRepr>>,
    // Outermost lifetime of the return ref, or None if the return type isn't
    // a ref. Set by lifetime elision (or copied from a user `'a` annotation).
    pub ret_lifetime: Option<LifetimeRepr>,
    // For methods (registered inside an `impl Target { ... }` block): the
    // impl's target type pattern. `None` for free functions. The pattern may
    // contain `RType::Param(impl_param_name)` slots that get bound by
    // matching against the receiver type at each call site.
    pub impl_target: Option<RType>,
    // Per `MethodCall` expression, indexed by Expr.id. Some(_) at MethodCall
    // node ids; None elsewhere.
    pub method_resolutions: Vec<Option<MethodResolution>>,
    // Per `Call` expression, indexed by Expr.id.
    pub call_resolutions: Vec<Option<CallResolution>>,
    // T4.6: places whose move-state at the binding's scope-end was non-Init,
    // snapshotted from borrowck's walk. Codegen consults this to decide what
    // to do at each Drop binding's drop point: `Init` means the binding
    // wasn't moved at all (unconditional drop); `Moved` means it was moved on
    // every path (skip drop); `MaybeMoved` means it was moved on some paths
    // (emit a runtime drop flag — set 1 at decl, 0 at every move site, drop
    // gated on flag).
    pub moved_places: Vec<MovedPlace>,
    // Per whole-binding move site: every (NodeId, binding-name) pair where
    // borrowck observed a non-Copy whole-binding read that consumed the
    // binding's storage. Codegen consults this to clear drop flags: at the
    // codegen for the matching NodeId, emit `flag = 0` for the named
    // binding (only when that binding's status at scope-end is MaybeMoved
    // — Init bindings don't have flags, and Moved bindings drop is just
    // skipped). Empty for fns with no whole-binding moves.
    pub move_sites: Vec<(crate::ast::NodeId, String)>,
    // Per-NodeId resolved type-args for builtin intrinsics whose codegen
    // depends on T (currently only `¤size_of::<T>()`). `Some` only at
    // those Builtin call sites; `None` everywhere else. Each inner Vec
    // holds the resolved RTypes in the order they appeared in the
    // turbofish. Codegen substitutes through the mono env before use.
    pub builtin_type_targets: Vec<Option<Vec<RType>>>,
    // Per-pattern.id ergonomics record. Default-zero entries describe
    // "no ergonomics applied"; non-zero entries tell mono how to rebuild
    // the pattern with explicit `&`/`ref` before codegen.
    pub pattern_ergo: Vec<PatternErgo>,
    // Per-NodeId closure info (sized to func.node_count). `Some(_)` at
    // each `ExprKind::Closure` site, `None` elsewhere. Populated during
    // typeck of the closure expression and consumed by the post-typeck
    // closure-lowering pass.
    pub closures: Vec<Option<ClosureInfo>>,
    // Per-NodeId bare-closure-call info (sized to func.node_count).
    // `Some(binding_name)` at `ExprKind::Call` sites where the callee
    // resolved to a local of closure type — typeck dispatched it as
    // `local.call((args,))` and mono lowers it as a `MethodCall`
    // MonoExpr using the binding's local + a synthesized args tuple.
    // None elsewhere.
    pub bare_closure_calls: Vec<Option<String>>,
    // RPIT slots: one entry per `impl Trait` occurrence in the
    // declared return type (in order found by depth-first walk).
    // Each slot's `bounds` are resolved at setup time; `pin` is the
    // concrete type the body returns at that position, recorded once
    // body-check completes. `Opaque { fn_path: self.path, slot }`
    // resolves to `pin` for layout queries; trait dispatch on the
    // Opaque consults `bounds`.
    pub rpit_slots: Vec<RpitSlot>,
    // Lifetime outlives predicates declared in the fn's where-clause
    // (`where 'a: 'b`). Phase B carry-only (see `GenericTemplate.lifetime_predicates`).
    pub lifetime_predicates: Vec<LifetimePredResolved>,
}

#[derive(Clone)]
pub struct RpitSlot {
    pub bounds: Vec<RpitBound>,
    pub pin: Option<RType>,
}

#[derive(Clone)]
pub struct RpitBound {
    pub trait_path: Vec<String>,
    pub trait_args: Vec<RType>,
    pub assoc_constraints: Vec<(String, RType)>,
}

// How a `Call` expression resolves to a callee. For non-generic functions
// it's an index into FuncTable.entries. For generic functions, it points to
// a template plus the type arguments at the call site (which may themselves
// contain `Param` if the calling function is also generic — substituted at
// monomorphization).
#[derive(Clone)]
pub enum CallResolution {
    Direct(usize),
    Generic {
        template_idx: usize,
        type_args: Vec<RType>,
    },
    // Enum variant construction: `Path::Variant(args...)` produces an
    // enum value. `enum_path` is the canonical enum's path; `disc` is
    // the variant index; `type_args` are the enum's type-args at this
    // construction site (substituted under any outer monomorphization
    // env at codegen time).
    Variant {
        enum_path: Vec<String>,
        disc: u32,
        type_args: Vec<RType>,
    },
}

// A generic function declaration. Its body is type-checked once,
// polymorphically (so let_types/lit_types/etc. may contain `RType::Param`).
// Codegen monomorphizes lazily per (template_idx, concrete type_args) pair,
// substituting Param → concrete in the recorded artifacts.
pub struct GenericTemplate {
    pub path: Vec<String>,
    pub type_params: Vec<String>,
    // Per type-param trait bounds (resolved to trait paths), in the same
    // order as `type_params`. Each inner Vec is the bound list for that
    // type-param. Used by symbolic trait-method dispatch in generic
    // bodies (`fn f<T: Show>(t: T) { t.show() }`).
    pub type_param_bounds: Vec<Vec<Vec<String>>>,
    // Per type-param, parallel to `type_param_bounds`: positional
    // trait-args at each bound site (resolved against the bound's
    // trait_type_params arity). Indexed `[param_idx][bound_idx]` →
    // `Vec<RType>`. Empty inner Vec when the bound is a non-generic
    // trait. Populated alongside `type_param_bounds` during setup;
    // used by bidirectional inference to read e.g. the `(u32,)` tuple
    // out of an `F: Fn(u32) -> u32` bound when a closure expression
    // is the matching call-site argument.
    pub type_param_bound_args: Vec<Vec<Vec<RType>>>,
    // Per type-param `Trait<Name = X, ...>` constraints, parallel to
    // `type_param_bounds`. Indexed `[param_idx][bound_idx][k]` →
    // `(assoc_name, ConcreteType)`. Empty vectors when no constraints.
    // Enforced at call sites: an inferred type-arg must satisfy each
    // bound's assoc constraints (impl's binding for `name` must equal
    // the constraint's type), otherwise a static "type mismatch on
    // associated type" error is raised.
    pub type_param_bound_assoc: Vec<Vec<Vec<(String, RType)>>>,
    // Number of leading entries in `type_params` that come from the
    // enclosing `impl<...>` block (the rest are the method's own type
    // params). Zero for free generic functions.
    pub impl_type_param_count: usize,
    // For trait-impl methods, the index into `TraitTable.impls`. None
    // for free fns and inherent methods.
    pub trait_impl_idx: Option<usize>,
    pub is_pub: bool,
    pub is_unsafe: bool,
    pub func: crate::ast::Function,
    pub enclosing_module: Vec<String>,
    pub source_file: String,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub expr_types: Vec<Option<RType>>,
    pub param_lifetimes: Vec<Option<LifetimeRepr>>,
    pub ret_lifetime: Option<LifetimeRepr>,
    // For impl methods: the impl's target type pattern (see FnSymbol).
    // `None` for free generic functions.
    pub impl_target: Option<RType>,
    pub method_resolutions: Vec<Option<MethodResolution>>,
    pub call_resolutions: Vec<Option<CallResolution>>,
    // T4.6: see FnSymbol.moved_places. For templates the snapshot is taken
    // from the polymorphic body walk and reused across monomorphizations
    // (move semantics don't depend on concrete type args).
    pub moved_places: Vec<MovedPlace>,
    // See FnSymbol.move_sites.
    pub move_sites: Vec<(crate::ast::NodeId, String)>,
    // See FnSymbol.builtin_type_targets.
    pub builtin_type_targets: Vec<Option<Vec<RType>>>,
    // See FnSymbol.pattern_ergo.
    pub pattern_ergo: Vec<PatternErgo>,
    // See FnSymbol.closures.
    pub closures: Vec<Option<ClosureInfo>>,
    // See FnSymbol.bare_closure_calls.
    pub bare_closure_calls: Vec<Option<String>>,
    // See FnSymbol.rpit_slots.
    pub rpit_slots: Vec<RpitSlot>,
    // Where-clause predicates whose LHS is a complex type (anything
    // other than a bare type-param). Param-LHS predicates are merged
    // into `type_param_bounds`/`type_param_bound_args`/`type_param_bound_assoc`
    // at setup time so they look indistinguishable from inline bounds.
    // Complex-LHS predicates are checked at each call site after the
    // type-param substitution is built: the LHS RType is substituted,
    // each bound is resolved via `solve_impl_in_ctx_with_args`, and
    // failure produces "where-clause predicate not satisfied".
    pub where_predicates: Vec<WherePredResolved>,
    // Lifetime outlives predicates from the function's where-clause
    // (`where 'a: 'b`). Phase B structural carry — see
    // `LifetimePredResolved`.
    pub lifetime_predicates: Vec<LifetimePredResolved>,
}

#[derive(Clone)]
pub struct WherePredResolved {
    pub lhs: RType,
    pub bounds: Vec<WhereBoundResolved>,
    pub span: crate::span::Span,
}

#[derive(Clone)]
pub struct WhereBoundResolved {
    pub trait_path: Vec<String>,
    pub trait_args: Vec<RType>,
    pub assoc_constraints: Vec<(String, RType)>,
}

// `'lhs: 'b1 + 'b2 + …` — outlives obligation. Phase B
// structural carry: validated for in-scope lifetimes at setup time
// and stored here for future borrowck phases to consume as outlives
// constraints. Today's borrowck doesn't yet do lifetime relation
// solving, so these are inert except for the in-scope check at
// setup.
#[derive(Clone)]
pub struct LifetimePredResolved {
    pub lhs: String,
    pub bounds: Vec<String>,
    pub span: crate::span::Span,
}

#[derive(Clone)]
pub struct MethodResolution {
    // For concrete methods (non-template), this is the WASM idx. For
    // generic-method calls, ignored — see `template_idx`/`type_args` instead.
    pub callee_idx: u32,
    pub callee_path: Vec<String>,
    pub recv_adjust: ReceiverAdjust,
    pub ret_borrows_receiver: bool,
    // When the method is a generic template (impl-generic and/or method-generic),
    // these record the resolution for codegen to monomorphize. type_args has
    // length = template's type_params.len(), in the same order: impl's params
    // first (bound to receiver type_args), then method's own (fresh vars
    // resolved by inference).
    pub template_idx: Option<usize>,
    pub type_args: Vec<RType>,
    // T2: deferred trait dispatch — populated when the call goes through
    // a `T: Trait` bound. Codegen substitutes `recv_type` against the
    // mono env and runs `solve_impl` to find the concrete impl + method.
    pub trait_dispatch: Option<TraitDispatch>,
}

#[derive(Clone)]
pub struct TraitDispatch {
    pub trait_path: Vec<String>,
    // Resolved positional trait-args (for `Mix<u32>`-style traits). Empty
    // for non-generic traits; `solve_impl_with_args` uses them alongside
    // `recv_type` to pick the right impl row at codegen / mono time.
    pub trait_args: Vec<RType>,
    pub method_name: String,
    pub recv_type: RType,
}

#[derive(Clone, Copy)]
pub enum ReceiverAdjust {
    Move,        // recv is consumed; method takes Self
    BorrowImm,   // recv is owned; method takes &Self → emit &recv
    BorrowMut,   // recv is owned; method takes &mut Self → emit &mut recv
    ByRef,       // recv is &Self/&mut Self; pass i32 directly (incl. mut→imm downgrade)
}

pub struct FuncTable {
    pub entries: Vec<FnSymbol>,
    pub templates: Vec<GenericTemplate>,
    // Per-impl-block bookkeeping for non-Path inherent impls
    // (`impl<T> *const T { … }`). Each entry's index is the synth idx
    // used in the methods' path prefix (`__inherent_synth_<idx>`).
    // `(file, span)` lets later passes recover the same idx via
    // `find_inherent_synth_idx`.
    pub inherent_synth_specs: Vec<(String, crate::span::Span)>,
    // Monotonically increasing counter used by typeck to allocate a
    // unique idx per closure expression. The closure's synthesized
    // struct path is `<enclosing_module>::__closure_<idx>`. Counter is
    // shared across all libraries/crates so paths never collide; reset
    // is unnecessary.
    pub closure_counter: u32,
}

pub fn find_inherent_synth_idx(
    funcs: &FuncTable,
    file: &str,
    span: &crate::span::Span,
) -> Option<usize> {
    let mut i = 0;
    while i < funcs.inherent_synth_specs.len() {
        let (f, s) = &funcs.inherent_synth_specs[i];
        if f == file
            && s.start.line == span.start.line
            && s.start.col == span.start.col
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn template_lookup<'a>(
    table: &'a FuncTable,
    path: &Vec<String>,
) -> Option<(usize, &'a GenericTemplate)> {
    let mut i = 0;
    while i < table.templates.len() {
        if &table.templates[i].path == path {
            return Some((i, &table.templates[i]));
        }
        i += 1;
    }
    None
}

pub fn func_lookup<'a>(table: &'a FuncTable, path: &Vec<String>) -> Option<&'a FnSymbol> {
    let mut i = 0;
    while i < table.entries.len() {
        if &table.entries[i].path == path {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}
