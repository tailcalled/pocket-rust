use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, MethodCall,
    Module, Stmt, StructLit,
};
use crate::span::{Error, Span};
use crate::typeck::{
    CallResolution, FuncTable, MethodResolution, MoveStatus, MovedPlace, RType, ReceiverAdjust,
    StructTable, TraitTable, find_lifetime_source, func_lookup, is_copy_with_bounds,
    template_lookup,
};

pub fn check(
    root: &Module,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let mut current_file = root.source_file.clone();
    let mut current_module: Vec<String> = Vec::new();
    push_root_name(&mut current_module, root);
    check_module(root, &mut current_module, &mut current_file, structs, enums, traits, funcs)?;
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn check_module(
    module: &Module,
    current_module: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => check_function(
                f,
                current_module,
                current_module,
                None,
                current_file,
                structs,
                enums,
                traits,
                funcs,
            )?,
            Item::Module(m) => {
                current_module.push(m.name.clone());
                check_module(m, current_module, current_file, structs, enums, traits, funcs)?;
                current_module.pop();
            }
            Item::Struct(_) => {}
            Item::Enum(_) => {}
            Item::Use(_) => {}
            Item::Impl(ib) => {
                let target_name = match &ib.target.kind {
                    crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
                        p.segments[0].name.clone()
                    }
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                let mut method_prefix = current_module.clone();
                method_prefix.push(target_name.clone());
                let mut target_full = current_module.clone();
                target_full.push(target_name);
                let mut impl_param_args: Vec<RType> = Vec::new();
                let mut k = 0;
                while k < ib.type_params.len() {
                    impl_param_args.push(RType::Param(ib.type_params[k].name.clone()));
                    k += 1;
                }
                let mut impl_lifetime_args: Vec<crate::typeck::LifetimeRepr> = Vec::new();
                let mut k = 0;
                while k < ib.lifetime_params.len() {
                    impl_lifetime_args.push(crate::typeck::LifetimeRepr::Named(
                        ib.lifetime_params[k].name.clone(),
                    ));
                    k += 1;
                }
                let target_rt = RType::Struct {
                    path: target_full,
                    type_args: impl_param_args,
                    lifetime_args: impl_lifetime_args,
                };
                let mut k = 0;
                while k < ib.methods.len() {
                    check_function(
                        &ib.methods[k],
                        current_module,
                        &method_prefix,
                        Some(target_rt.clone()),
                        current_file,
                        structs,
                        enums,
                        traits,
                        funcs,
                    )?;
                    k += 1;
                }
            }
            Item::Trait(_) => {}
        }
        i += 1;
    }
    *current_file = saved;
    Ok(())
}

fn check_function(
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<RType>,
    current_file: &str,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let mut full = path_prefix.clone();
    full.push(func.name.clone());

    // Walk the body using an immutable view of `funcs`; capture state.moved
    // from the resulting BorrowState, then drop the view so we can mutate
    // `funcs` to write back the moved snapshot (T4.6).
    let moved: Vec<MovedPlace>;
    let move_sites: Vec<(crate::ast::NodeId, String)>;
    let cfg_moved: Vec<MovedPlace>;
    let cfg_move_sites: Vec<(crate::ast::NodeId, String)>;
    {
        let funcs_ro: &FuncTable = &*funcs;
        // The function may be a regular entry or a generic template — peel both.
        let (param_types, expr_types, method_resolutions, call_resolutions, type_params, type_param_bounds) =
            if let Some(entry) = func_lookup(funcs_ro, &full) {
                (
                    &entry.param_types,
                    &entry.expr_types,
                    &entry.method_resolutions,
                    &entry.call_resolutions,
                    Vec::<String>::new(),
                    Vec::<Vec<Vec<String>>>::new(),
                )
            } else if let Some((_, t)) = template_lookup(funcs_ro, &full) {
                let mut bounds_clone: Vec<Vec<Vec<String>>> = Vec::new();
                let mut i = 0;
                while i < t.type_param_bounds.len() {
                    let mut row: Vec<Vec<String>> = Vec::new();
                    let mut j = 0;
                    while j < t.type_param_bounds[i].len() {
                        row.push(t.type_param_bounds[i][j].clone());
                        j += 1;
                    }
                    bounds_clone.push(row);
                    i += 1;
                }
                (
                    &t.param_types,
                    &t.expr_types,
                    &t.method_resolutions,
                    &t.call_resolutions,
                    t.type_params.clone(),
                    bounds_clone,
                )
            } else {
                unreachable!("typeck registered this function");
            };

        let liveness = compute_liveness(&func.body, &func.params);
        let mut state = BorrowState {
            holders: Vec::new(),
            moved: Vec::new(),
            move_sites: Vec::new(),
            expr_types,
            method_resolutions,
            call_resolutions,
            file: current_file.to_string(),
            funcs: funcs_ro,
            traits,
            structs,
            enums,
            current_module,
            self_target,
            type_params,
            type_param_bounds,
            liveness,
            current_step: 0,
        };

        let mut k = 0;
        while k < func.params.len() {
            state.holders.push(Holder {
                name: Some(func.params[k].name.clone()),
                rtype: Some(param_types[k].clone()),
                holds: Vec::new(),
                field_holds: Vec::new(),
            });
            k += 1;
        }

        walk_stmts_and_tail(&mut state, &func.body)?;
        moved = state.moved;
        move_sites = state.move_sites;

        // Shadow validation: build a CFG for this function. Failure
        // here means a phase-1/1.5 bug — surfaces immediately.
        let return_ty = match func_lookup(funcs_ro, &full) {
            Some(e) => e
                .return_type
                .clone()
                .unwrap_or(crate::typeck::RType::Tuple(Vec::new())),
            None => template_lookup(funcs_ro, &full)
                .map(|(_, t)| {
                    t.return_type
                        .clone()
                        .unwrap_or(crate::typeck::RType::Tuple(Vec::new()))
                })
                .unwrap(),
        };
        let cfg_ctx = crate::cfg_build::CfgBuildCtx {
            structs,
            enums,
            traits,
            funcs: funcs_ro,
            expr_types: state.expr_types,
            method_resolutions: state.method_resolutions,
            call_resolutions: state.call_resolutions,
            type_params: &state.type_params,
            type_param_bounds: &state.type_param_bounds,
            param_types,
            return_type: &return_ty,
        };
        let cfg = crate::cfg_build::build(func, &cfg_ctx);
        // Phase 2 shadow: run move dataflow on every function. Any
        // error from CFG on an AST-accepted program is a false positive.
        let move_analysis = crate::cfg_moves::analyze(&cfg, current_file);
        if !move_analysis.errors.is_empty() {
            panic!(
                "CFG move analysis false positive in `{}`: {:?}",
                func.name,
                move_analysis
                    .errors
                    .iter()
                    .map(|e| format!(
                        "{}:{}:{}: {}",
                        e.file, e.span.start.line, e.span.start.col, e.message
                    ))
                    .collect::<Vec<_>>()
            );
        }
        // Phase 3 shadow: run liveness. No errors expected — just
        // ensures the analysis terminates on every function.
        let liveness = crate::cfg_liveness::analyze(&cfg);
        // Phase 4 shadow: NLL borrow checking. Like move analysis,
        // any error on an AST-accepted program is a false positive.
        let borrow_check = crate::cfg_borrows::analyze(&cfg, &liveness, current_file);
        if !borrow_check.errors.is_empty() {
            panic!(
                "CFG borrow check false positive in `{}`: {:?}",
                func.name,
                borrow_check
                    .errors
                    .iter()
                    .map(|e| format!(
                        "{}:{}:{}: {}",
                        e.file, e.span.start.line, e.span.start.col, e.message
                    ))
                    .collect::<Vec<_>>()
            );
        }
        // Phase 6 step 1: drop-flag data comes from the CFG analysis
        // — codegen reads moved_places and move_sites from FnSymbol,
        // and we now write CFG-derived versions there (replacing the
        // AST walker's). A validation step (`validate_drop_flags`)
        // still runs to catch divergences, panicking on mismatch so
        // any regression surfaces immediately.
        validate_drop_flags(
            &func.name,
            &cfg,
            &move_analysis,
            &moved,
            &move_sites,
            traits,
        );
        cfg_moved = build_moved_places(&cfg, &move_analysis);
        cfg_move_sites = move_analysis.move_sites.clone();
    }

    // Snapshot moved places + move-sites onto the function's metadata
    // so codegen knows what status each binding had at scope-end and
    // where the moves happened (for drop-flag clearing). Phase 6
    // tried switching to CFG-derived data; turned out to introduce
    // codegen regressions in cases unrelated to drop flags (the
    // CFG move analysis records spurious move sites for non-Drop
    // bindings that codegen's drop-flag-clearing path was sensitive
    // to). For now, keep AST-walker output as the source of truth;
    // CFG data is still computed and validated against AST output as
    // a sanity check.
    let _ = (cfg_moved, cfg_move_sites);
    let mut k = 0;
    while k < funcs.entries.len() {
        if funcs.entries[k].path == full {
            funcs.entries[k].moved_places = moved;
            funcs.entries[k].move_sites = move_sites;
            return Ok(());
        }
        k += 1;
    }
    let mut k = 0;
    while k < funcs.templates.len() {
        if funcs.templates[k].path == full {
            funcs.templates[k].moved_places = moved;
            funcs.templates[k].move_sites = move_sites;
            return Ok(());
        }
        k += 1;
    }
    unreachable!("typeck registered this function");
}

// Convert the CFG move analysis's `MovedLocal` entries into the
// `MovedPlace` shape codegen consumes. Each MovedLocal becomes a
// single-segment place keyed by the local's name.
fn build_moved_places(
    cfg: &crate::cfg::Cfg,
    move_analysis: &crate::cfg_moves::MoveAnalysis,
) -> Vec<MovedPlace> {
    let mut out: Vec<MovedPlace> = Vec::new();
    let mut i = 0;
    while i < move_analysis.moved_locals.len() {
        let m = &move_analysis.moved_locals[i];
        if let Some(name) = &cfg.locals[m.local as usize].name {
            let status = match m.status {
                crate::cfg_moves::MoveStatus::Moved => crate::typeck::MoveStatus::Moved,
                crate::cfg_moves::MoveStatus::MaybeMoved => {
                    crate::typeck::MoveStatus::MaybeMoved
                }
            };
            out.push(MovedPlace {
                place: vec![name.clone()],
                status,
            });
        }
        i += 1;
    }
    out
}

// Compare the CFG move analysis's drop-flag outputs against the AST
// walker's, filtered to Drop-typed locals (the only ones that matter
// for codegen — non-Drop locals' moved status is unused). Sub-place
// AST entries are filtered out (whole-binding only).
//
// This catches semantic divergences between the two implementations on
// data that codegen actually consumes; benign disagreements on
// non-Drop locals (e.g., the AST walker's "&mut T moves on read" quirk)
// are tolerated.
fn validate_drop_flags(
    func_name: &str,
    cfg: &crate::cfg::Cfg,
    move_analysis: &crate::cfg_moves::MoveAnalysis,
    ast_moved: &Vec<MovedPlace>,
    ast_move_sites: &Vec<(crate::ast::NodeId, String)>,
    traits: &TraitTable,
) {
    use crate::typeck::is_drop;

    // Helper: look up a binding's type from the cfg's locals by name.
    let local_drop = |name: &str| -> bool {
        let mut i = 0;
        while i < cfg.locals.len() {
            if cfg.locals[i].name.as_deref() == Some(name) {
                return is_drop(&cfg.locals[i].ty, traits);
            }
            i += 1;
        }
        false
    };

    // Build filtered AST whole-binding entries.
    let mut ast_whole: Vec<(String, crate::typeck::MoveStatus)> = Vec::new();
    let mut i = 0;
    while i < ast_moved.len() {
        if ast_moved[i].place.len() == 1 && local_drop(&ast_moved[i].place[0]) {
            ast_whole.push((ast_moved[i].place[0].clone(), ast_moved[i].status.clone()));
        }
        i += 1;
    }

    // Build filtered CFG entries.
    let mut cfg_whole: Vec<(String, crate::typeck::MoveStatus)> = Vec::new();
    let mut i = 0;
    while i < move_analysis.moved_locals.len() {
        let m = &move_analysis.moved_locals[i];
        if let Some(name) = &cfg.locals[m.local as usize].name {
            if is_drop(&cfg.locals[m.local as usize].ty, traits) {
                let status = match m.status {
                    crate::cfg_moves::MoveStatus::Moved => crate::typeck::MoveStatus::Moved,
                    crate::cfg_moves::MoveStatus::MaybeMoved => {
                        crate::typeck::MoveStatus::MaybeMoved
                    }
                };
                cfg_whole.push((name.clone(), status));
            }
        }
        i += 1;
    }

    // Filter move sites to Drop locals too.
    let ast_sites_drop: Vec<(crate::ast::NodeId, String)> = ast_move_sites
        .iter()
        .filter(|(_, n)| local_drop(n))
        .cloned()
        .collect();
    let cfg_sites_drop: Vec<(crate::ast::NodeId, String)> = move_analysis
        .move_sites
        .iter()
        .filter(|(_, n)| local_drop(n))
        .cloned()
        .collect();

    let same_moved = sets_equal_moved(&ast_whole, &cfg_whole);
    let same_sites = sets_equal_sites(&ast_sites_drop, &cfg_sites_drop);
    if !same_moved || !same_sites {
        panic!(
            "drop-flag mismatch in `{}`:\n  AST moved (Drop): {:?}\n  CFG moved (Drop): {:?}\n  AST sites (Drop): {:?}\n  CFG sites (Drop): {:?}",
            func_name, ast_whole, cfg_whole, ast_sites_drop, cfg_sites_drop
        );
    }
}

fn sets_equal_moved(
    a: &Vec<(String, crate::typeck::MoveStatus)>,
    b: &Vec<(String, crate::typeck::MoveStatus)>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !b.iter().any(|x| x.0 == a[i].0 && x.1 == a[i].1) {
            return false;
        }
        i += 1;
    }
    true
}

fn sets_equal_sites(
    a: &Vec<(crate::ast::NodeId, String)>,
    b: &Vec<(crate::ast::NodeId, String)>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !b.iter().any(|x| x.0 == a[i].0 && x.1 == a[i].1) {
            return false;
        }
        i += 1;
    }
    true
}

// ---------- State ----------

struct BorrowState<'a> {
    // Stack of holders. A holder either names a let/param binding (Some name)
    // or is a synthetic call slot (None name). Each holder records the
    // borrows it currently keeps alive (a list of place paths).
    holders: Vec<Holder>,
    // Move state per place: each entry says "this place is moved (or
    // maybe-moved) right now." Reads of any such place are rejected.
    // Only Moved appears in straight-line code; MaybeMoved comes from
    // join points (if/else, later match/loops). The implicit Init state
    // is "no entry."
    moved: Vec<MovedPlace>,
    // Whole-binding move-site annotations: `(NodeId of the Var
    // expression, binding name)`. Codegen consults these to clear drop
    // flags at the right point. We record every move site (whether or
    // not the binding ends up MaybeMoved) — codegen filters by which
    // bindings actually got flags allocated.
    move_sites: Vec<(crate::ast::NodeId, String)>,
    // Per-NodeId resolved types/resolutions populated by typeck. Borrowck
    // looks up by `expr.id` rather than maintaining a source-DFS counter.
    expr_types: &'a Vec<Option<RType>>,
    method_resolutions: &'a Vec<Option<MethodResolution>>,
    call_resolutions: &'a Vec<Option<CallResolution>>,
    file: String,
    funcs: &'a FuncTable,
    traits: &'a TraitTable,
    structs: &'a StructTable,
    enums: &'a crate::typeck::EnumTable,
    type_params: Vec<String>,
    type_param_bounds: Vec<Vec<Vec<String>>>,
    #[allow(dead_code)]
    current_module: &'a Vec<String>,
    #[allow(dead_code)]
    self_target: Option<RType>,
    // Liveness — name → last-use step, computed by a pre-pass over the body.
    // Holders whose binding's last_use < current_step have their borrows
    // garbage-collected (cleared) after each step.
    liveness: Liveness,
    current_step: u32,
}

struct Liveness {
    last_use: Vec<(String, u32)>,
}

struct Holder {
    name: Option<String>,
    rtype: Option<RType>,
    holds: Vec<HeldBorrow>,
    // Per-slot borrows for struct-typed bindings whose fields hold refs.
    // Each entry tags a field path with the borrows tied to that slot. A
    // read of `binding.field` (where field is ref-typed) returns the
    // matching entry's borrows; moving the binding transfers them to the
    // new holder.
    field_holds: Vec<FieldHold>,
}

struct HeldBorrow {
    place: Vec<String>,
    mutable: bool,
}

// One per-slot record: a multi-segment field path within a struct holder,
// plus the borrows tied to that slot. A single-segment path like
// `["r"]` records a top-level ref field; a nested path like `["b", "r"]`
// records a ref reachable through an inner struct field.
struct FieldHold {
    field: Vec<String>,
    borrows: Vec<HeldBorrow>,
}

// A descriptor of the borrows a value carries forward — i.e. which places this
// expression's value, if it's a reference, refers to. For non-reference values,
// `borrows` is empty; if the value is a struct with ref fields,
// `field_borrows` records the per-slot borrows so a binding holder can
// preserve them under the per-slot tracking model. The caller decides what
// to do with these (absorb into a binding, attach to a call slot, drop).
struct ValueDesc {
    borrows: Vec<HeldBorrow>,
    field_borrows: Vec<FieldHold>,
}

fn empty_desc() -> ValueDesc {
    ValueDesc {
        borrows: Vec::new(),
        field_borrows: Vec::new(),
    }
}

fn clone_field_holds(v: &Vec<FieldHold>) -> Vec<FieldHold> {
    let mut out: Vec<FieldHold> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(FieldHold {
            field: v[i].field.clone(),
            borrows: clone_held_borrows(&v[i].borrows),
        });
        i += 1;
    }
    out
}

// Slice equality for the field-path of a FieldHold, used when looking up
// nested per-slot borrows from a multi-segment field-access chain.
fn field_path_matches(field: &Vec<String>, sub: &[String]) -> bool {
    if field.len() != sub.len() {
        return false;
    }
    let mut i = 0;
    while i < field.len() {
        if field[i] != sub[i] {
            return false;
        }
        i += 1;
    }
    true
}

// ---------- Walk ----------

fn walk_stmts_and_tail(state: &mut BorrowState, block: &Block) -> Result<ValueDesc, Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => walk_let_stmt(state, let_stmt)?,
            Stmt::Assign(assign) => walk_assign_stmt(state, assign)?,
            Stmt::Expr(expr) => {
                walk_expr(state, expr)?;
            }
            Stmt::Use(_) => {}
        }
        state.current_step += 1;
        gc_dead_holders(state);
        i += 1;
    }
    match &block.tail {
        Some(tail) => {
            let desc = walk_expr(state, tail)?;
            state.current_step += 1;
            gc_dead_holders(state);
            Ok(desc)
        }
        None => Ok(empty_desc()),
    }
}

// After each step, holders whose binding's last-use step is strictly less than
// `current_step` are no longer live — their borrows are dropped. Implements
// straight-line NLL: a borrow lives until the binding's last use, not until
// scope end.
fn gc_dead_holders(state: &mut BorrowState) {
    let mut i = 0;
    while i < state.holders.len() {
        if let Some(name) = &state.holders[i].name {
            let lu = liveness_lookup(&state.liveness, name);
            match lu {
                Some(s) if s >= state.current_step => {}
                _ => state.holders[i].holds.clear(),
            }
        }
        i += 1;
    }
}

fn liveness_lookup(info: &Liveness, name: &str) -> Option<u32> {
    let mut i = 0;
    while i < info.last_use.len() {
        if info.last_use[i].0 == name {
            return Some(info.last_use[i].1);
        }
        i += 1;
    }
    None
}

fn liveness_record(info: &mut Liveness, name: &str, step: u32) {
    let mut i = 0;
    while i < info.last_use.len() {
        if info.last_use[i].0 == name {
            if info.last_use[i].1 < step {
                info.last_use[i].1 = step;
            }
            return;
        }
        i += 1;
    }
    info.last_use.push((name.to_string(), step));
}

fn compute_liveness(body: &Block, params: &Vec<crate::ast::Param>) -> Liveness {
    let mut info = Liveness {
        last_use: Vec::new(),
    };
    // Seed each parameter at step 0 — their borrows are held by holders from
    // the start; the GC pass should keep them around until they're actually
    // referenced (or longer if referenced later).
    let mut i = 0;
    while i < params.len() {
        liveness_record(&mut info, &params[i].name, 0);
        i += 1;
    }
    let mut step: u32 = 0;
    walk_block_for_liveness(body, &mut step, &mut info);
    info
}

fn walk_block_for_liveness(block: &Block, step: &mut u32, info: &mut Liveness) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => {
                walk_expr_for_liveness(&let_stmt.value, step, info);
                // Anchor the new binding's lifetime at the let-stmt's step;
                // later reads bump it. Without this, an unused binding would
                // never appear in `last_use` and `liveness_lookup` would return
                // None — which the GC treats as "dead immediately." That's
                // the desired behavior, but recording it explicitly keeps the
                // semantics readable.
                liveness_record(info, &let_stmt.name, *step);
            }
            Stmt::Assign(assign) => {
                walk_expr_for_liveness(&assign.lhs, step, info);
                walk_expr_for_liveness(&assign.rhs, step, info);
            }
            Stmt::Expr(expr) => walk_expr_for_liveness(expr, step, info),
            Stmt::Use(_) => {}
        }
        *step += 1;
        i += 1;
    }
    if let Some(tail) = &block.tail {
        walk_expr_for_liveness(tail, step, info);
        *step += 1;
    }
}

fn walk_expr_for_liveness(expr: &Expr, step: &mut u32, info: &mut Liveness) {
    match &expr.kind {
        ExprKind::IntLit(_) => {}
        ExprKind::Var(name) => liveness_record(info, name, *step),
        ExprKind::Borrow { inner, .. } => walk_expr_for_liveness(inner, step, info),
        ExprKind::FieldAccess(fa) => walk_expr_for_liveness(&fa.base, step, info),
        ExprKind::Cast { inner, .. } => walk_expr_for_liveness(inner, step, info),
        ExprKind::Deref(inner) => walk_expr_for_liveness(inner, step, info),
        ExprKind::Call(c) => {
            let mut i = 0;
            while i < c.args.len() {
                walk_expr_for_liveness(&c.args[i], step, info);
                i += 1;
            }
        }
        ExprKind::StructLit(s) => {
            let mut i = 0;
            while i < s.fields.len() {
                walk_expr_for_liveness(&s.fields[i].value, step, info);
                i += 1;
            }
        }
        ExprKind::MethodCall(mc) => {
            walk_expr_for_liveness(&mc.receiver, step, info);
            let mut i = 0;
            while i < mc.args.len() {
                walk_expr_for_liveness(&mc.args[i], step, info);
                i += 1;
            }
        }
        ExprKind::Block(b) | ExprKind::Unsafe(b) => {
            // Inner block stmts share the same step counter as the outer walk —
            // borrowck's actual walk also advances `current_step` inside inner
            // blocks (via walk_stmts_and_tail), so the two passes stay in sync.
            walk_block_for_liveness(b.as_ref(), step, info);
        }
        ExprKind::BoolLit(_) => {}
        ExprKind::If(if_expr) => {
            // Condition runs first, then exactly one arm. Liveness here is
            // conservative: we visit both arms in sequence, so a binding's
            // last-use is `max(arm1, arm2)` — keeps it alive long enough
            // for either path. This means a borrow created in the cond and
            // only consumed in one arm stays live across both arms, which
            // is correct (the borrowck walker uses snapshot/merge to track
            // moves, but liveness GC stays conservative).
            walk_expr_for_liveness(&if_expr.cond, step, info);
            walk_block_for_liveness(if_expr.then_block.as_ref(), step, info);
            walk_block_for_liveness(if_expr.else_block.as_ref(), step, info);
        }
        ExprKind::Builtin { args, .. } => {
            let mut i = 0;
            while i < args.len() {
                walk_expr_for_liveness(&args[i], step, info);
                i += 1;
            }
        }
        ExprKind::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                walk_expr_for_liveness(&elems[i], step, info);
                i += 1;
            }
        }
        ExprKind::TupleIndex { base, .. } => {
            walk_expr_for_liveness(base, step, info);
        }
        ExprKind::Match(m) => {
            walk_expr_for_liveness(&m.scrutinee, step, info);
            let mut i = 0;
            while i < m.arms.len() {
                if let Some(g) = &m.arms[i].guard {
                    walk_expr_for_liveness(g, step, info);
                }
                // Pattern bindings are seeded so unused arm-body lookups
                // don't think the binding's last-use is "before its
                // declaration" (which would be a degenerate case anyway,
                // but seeding is harmless and matches the let-stmt path).
                let mut names: Vec<String> = Vec::new();
                collect_pattern_bindings(&m.arms[i].pattern, &mut names);
                let mut k = 0;
                while k < names.len() {
                    liveness_record(info, &names[k], *step);
                    k += 1;
                }
                walk_expr_for_liveness(&m.arms[i].body, step, info);
                i += 1;
            }
        }
        ExprKind::IfLet(il) => {
            walk_expr_for_liveness(&il.scrutinee, step, info);
            walk_block_for_liveness(il.then_block.as_ref(), step, info);
            walk_block_for_liveness(il.else_block.as_ref(), step, info);
        }
        ExprKind::While(w) => {
            walk_expr_for_liveness(&w.cond, step, info);
            walk_block_for_liveness(w.body.as_ref(), step, info);
        }
        ExprKind::Break { .. } | ExprKind::Continue { .. } => {}
    }
}

fn walk_assign_stmt(state: &mut BorrowState, assign: &AssignStmt) -> Result<(), Error> {
    // Deref-rooted writes (`*p = …;`, `(*p).f = …;`): writing through a
    // ref/raw-ptr exclusively (`&mut`/`*mut`) is authorized by typeck. Borrow
    // tracking can't precisely identify the underlying place (we'd need alias
    // analysis), so we just walk the inner deref target and the RHS for their
    // side effects and skip the conflict scan.
    if is_deref_rooted_assign(&assign.lhs) {
        walk_assign_lhs(state, &assign.lhs)?;
        walk_expr(state, &assign.rhs)?;
        return Ok(());
    }
    let chain = extract_place(&assign.lhs)
        .expect("typeck verified the assignment LHS is a place expression");
    // Reject if any holder has an overlapping path — assignment can't proceed
    // while the target memory is borrowed.
    // Skip the conflict scan when the assignment is *through* a `&mut` binding —
    // the borrow on that binding is the very thing that authorizes the write.
    let through_mut_ref = if chain.len() > 1 {
        let mut found: Option<usize> = None;
        let mut i = state.holders.len();
        while i > 0 {
            i -= 1;
            if let Some(n) = &state.holders[i].name {
                if n == &chain[0] {
                    found = Some(i);
                    break;
                }
            }
        }
        match found {
            Some(idx) => matches!(
                state.holders[idx].rtype,
                Some(RType::Ref { mutable: true, .. })
            ),
            None => false,
        }
    } else {
        false
    };
    if !through_mut_ref {
        let mut h = 0;
        while h < state.holders.len() {
            let mut k = 0;
            while k < state.holders[h].holds.len() {
                if paths_share_prefix(&state.holders[h].holds[k].place, &chain) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot assign to `{}` while it is borrowed",
                            place_to_string(&chain)
                        ),
                        span: assign.span.copy(),
                    });
                }
                k += 1;
            }
            h += 1;
        }
    }
    // Walk the RHS for its move/borrow effects.
    let desc = walk_expr(state, &assign.rhs)?;
    // RHS desc would carry borrows iff the result is a ref. Assignment to a
    // non-ref binding can't accept ref-typed values (typeck enforced); assignment
    // to a ref binding (e.g. `let mut r: &T; r = …;`) treats the new value the
    // same way the binding's `let` would have. For simplicity, drop the desc
    // here — the binding is already a holder, and reassignment doesn't change
    // which holder owns existing borrows. (This means once-borrowed-always-tied
    // for a ref binding; we can refine later.)
    let _ = desc;
    // The assigned place is now fresh; clear any move records on it or below.
    let mut new_moved: Vec<MovedPlace> = Vec::new();
    let mut i = 0;
    while i < state.moved.len() {
        if !chain_is_prefix_of(&chain, &state.moved[i].place) {
            new_moved.push(state.moved[i].clone());
        }
        i += 1;
    }
    state.moved = new_moved;
    Ok(())
}

fn chain_is_prefix_of(prefix: &Vec<String>, full: &Vec<String>) -> bool {
    if prefix.len() > full.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if prefix[i] != full[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn walk_let_stmt(state: &mut BorrowState, let_stmt: &LetStmt) -> Result<(), Error> {
    let desc = walk_expr(state, &let_stmt.value)?;
    let ty = state.expr_types[let_stmt.value.id as usize]
        .as_ref()
        .expect("typeck recorded this binding's type")
        .clone();
    state.holders.push(Holder {
        name: Some(let_stmt.name.clone()),
        rtype: Some(ty),
        holds: desc.borrows,
        field_holds: desc.field_borrows,
    });
    Ok(())
}

fn walk_expr(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    match &expr.kind {
        ExprKind::IntLit(_) => Ok(empty_desc()),
        ExprKind::Var(name) => walk_var(state, name, expr),
        ExprKind::Call(call) => walk_call(state, call, expr.id),
        ExprKind::StructLit(lit) => walk_struct_lit(state, lit),
        ExprKind::FieldAccess(fa) => walk_field_access(state, fa, expr),
        ExprKind::Borrow { .. } => walk_borrow(state, expr),
        ExprKind::Cast { inner, .. } => {
            // The inner produces side effects (moves, registered borrows) that
            // we still want to surface, but the cast itself yields a raw
            // pointer with no compile-time lifetime tracking — drop the
            // borrows so they don't get re-attached downstream.
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
        ExprKind::Deref(inner) => {
            // Deref reads through a ref/raw-ptr and yields the pointed-at
            // value. Refs/raw-ptrs are Copy, so reading them clones the
            // borrow handle — but typeck rejects deref of non-Copy inner, so
            // the resulting value carries no borrows of its own.
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
        ExprKind::Unsafe(block) => walk_block_expr(state, block.as_ref()),
        ExprKind::Block(block) => walk_block_expr(state, block.as_ref()),
        ExprKind::MethodCall(mc) => walk_method_call(state, mc, expr.id),
        ExprKind::BoolLit(_) => Ok(empty_desc()),
        ExprKind::If(if_expr) => walk_if_expr(state, if_expr),
        ExprKind::Builtin { args, .. } => {
            // Builtins consume their args by value (Copy primitives —
            // ints, bools). Walk for side effects but produce no
            // borrows: the result is a fresh primitive value.
            let mut i = 0;
            while i < args.len() {
                walk_expr(state, &args[i])?;
                i += 1;
            }
            Ok(empty_desc())
        }
        ExprKind::Tuple(elems) => walk_tuple(state, elems),
        ExprKind::TupleIndex { base, index, .. } => {
            walk_tuple_index(state, base, *index, expr)
        }
        ExprKind::Match(m) => walk_match_expr(state, m),
        ExprKind::IfLet(il) => walk_if_let_expr(state, il),
        // While/Break/Continue: the AST walker doesn't handle these
        // — functions using loops are routed to the CFG borrowck.
        // Bypass here so the function still typechecks at this layer
        // (compile-only smoke test); the CFG produces the real errors.
        ExprKind::While(_) | ExprKind::Break { .. } | ExprKind::Continue { .. } => {
            Ok(empty_desc())
        }
    }
}

fn walk_match_expr(
    state: &mut BorrowState,
    m: &crate::ast::MatchExpr,
) -> Result<ValueDesc, Error> {
    // Resolve the scrutinee type (substituted under any outer
    // monomorphization env). Pattern processing uses this as the
    // "type at the top of the pattern" and descends as it peels
    // layers.
    let scrut_ty: RType = state.expr_types[m.scrutinee.id as usize]
        .clone()
        .unwrap_or(RType::Tuple(Vec::new()));
    // Try to identify the scrutinee as a place expression. If yes,
    // pattern bindings can record borrows / partial moves rooted at
    // that place. If the scrutinee is an rvalue (call, struct lit,
    // etc.), we walk it for side effects and pass `None` to the
    // pattern walker so no place-rooted tracking happens.
    let scrut_place: Option<Vec<String>> = extract_place(&m.scrutinee);
    if let Some(p) = &scrut_place {
        // Don't pre-emptively move/borrow — defer to pattern walking.
        // But check the place hasn't already been moved (matching
        // a moved value is a hard error).
        check_not_moved(state, p, &m.scrutinee.span)?;
    } else {
        // Rvalue scrutinee: surface side effects and let any borrows
        // it produces die at a synthetic call slot.
        let scrut_slot = state.holders.len();
        state.holders.push(Holder {
            name: None,
            rtype: None,
            holds: Vec::new(),
            field_holds: Vec::new(),
        });
        let scrut_desc = walk_expr(state, &m.scrutinee)?;
        let scrut_holds = HeldBorrow_vec_from_desc(&scrut_desc);
        state.holders[scrut_slot].holds = scrut_holds;
        state.holders.truncate(scrut_slot);
    }
    if m.arms.is_empty() {
        return Ok(empty_desc());
    }
    let pre_moved: Vec<MovedPlace> = clone_moved_vec(&state.moved);
    let pre_holders_len = state.holders.len();
    let pre_holders_state = snapshot_holders_state(state);
    let mut merged_moved: Option<Vec<MovedPlace>> = None;
    let mut tail_borrows: Vec<HeldBorrow> = Vec::new();
    let mut tail_field_borrows: Vec<FieldHold> = Vec::new();
    let mut i = 0;
    while i < m.arms.len() {
        // Reset to the pre-arm state for each arm.
        state.moved = clone_moved_vec(&pre_moved);
        state.holders.truncate(pre_holders_len);
        restore_holders_state(state, snapshot_clone(&pre_holders_state));
        let mark = state.holders.len();
        // Walk the pattern: pushes bindings as Holders with proper
        // rtype and (for `ref` bindings) outstanding borrows; records
        // partial moves for value-bindings of non-Copy values rooted
        // at the scrutinee place.
        walk_pattern_for_borrowck(
            state,
            &m.arms[i].pattern,
            &scrut_ty,
            scrut_place.as_ref(),
            &m.arms[i].pattern.span,
        )?;
        if let Some(g) = &m.arms[i].guard {
            walk_expr(state, g)?;
        }
        let arm_desc = walk_expr(state, &m.arms[i].body)?;
        state.holders.truncate(mark);
        let arm_moved = clone_moved_vec(&state.moved);
        merged_moved = Some(match merged_moved {
            Some(prev) => merge_moved_sets(&prev, &arm_moved),
            None => arm_moved,
        });
        let mut k = 0;
        while k < arm_desc.borrows.len() {
            tail_borrows.push(HeldBorrow {
                place: arm_desc.borrows[k].place.clone(),
                mutable: arm_desc.borrows[k].mutable,
            });
            k += 1;
        }
        let mut k = 0;
        while k < arm_desc.field_borrows.len() {
            tail_field_borrows.push(FieldHold {
                field: arm_desc.field_borrows[k].field.clone(),
                borrows: clone_held_borrows(&arm_desc.field_borrows[k].borrows),
            });
            k += 1;
        }
        i += 1;
    }
    state.moved = merged_moved.unwrap_or(pre_moved);
    Ok(ValueDesc {
        borrows: tail_borrows,
        field_borrows: tail_field_borrows,
    })
}

// `if let Pat = scrut { then } else { else }`. Like a single-arm
// match plus an else fallback. Walks the scrutinee, then both arms
// against a snapshot, and merges the moved-place sets across the
// two paths. Pattern bindings (with their borrows / partial moves)
// scope to the then-arm only.
fn walk_if_let_expr(
    state: &mut BorrowState,
    il: &crate::ast::IfLetExpr,
) -> Result<ValueDesc, Error> {
    let scrut_ty: RType = state.expr_types[il.scrutinee.id as usize]
        .clone()
        .unwrap_or(RType::Tuple(Vec::new()));
    let scrut_place: Option<Vec<String>> = extract_place(&il.scrutinee);
    if let Some(p) = &scrut_place {
        check_not_moved(state, p, &il.scrutinee.span)?;
    } else {
        let scrut_slot = state.holders.len();
        state.holders.push(Holder {
            name: None,
            rtype: None,
            holds: Vec::new(),
            field_holds: Vec::new(),
        });
        let scrut_desc = walk_expr(state, &il.scrutinee)?;
        let scrut_holds = HeldBorrow_vec_from_desc(&scrut_desc);
        state.holders[scrut_slot].holds = scrut_holds;
        state.holders.truncate(scrut_slot);
    }
    let pre_moved: Vec<MovedPlace> = clone_moved_vec(&state.moved);
    let pre_holders_len = state.holders.len();
    let pre_holders_state = snapshot_holders_state(state);
    // Then-arm: pattern bindings + body.
    let mark = state.holders.len();
    walk_pattern_for_borrowck(
        state,
        &il.pattern,
        &scrut_ty,
        scrut_place.as_ref(),
        &il.pattern.span,
    )?;
    let then_desc = walk_block_expr(state, il.then_block.as_ref())?;
    state.holders.truncate(mark);
    let then_moved = clone_moved_vec(&state.moved);
    // Else-arm: no bindings, fresh state.
    state.moved = clone_moved_vec(&pre_moved);
    state.holders.truncate(pre_holders_len);
    restore_holders_state(state, snapshot_clone(&pre_holders_state));
    let else_desc = walk_block_expr(state, il.else_block.as_ref())?;
    let else_moved = clone_moved_vec(&state.moved);
    state.moved = merge_moved_sets(&then_moved, &else_moved);
    // Combine borrows from both arms (caller decides what to do with them).
    let mut borrows = then_desc.borrows;
    let mut k = 0;
    while k < else_desc.borrows.len() {
        borrows.push(HeldBorrow {
            place: else_desc.borrows[k].place.clone(),
            mutable: else_desc.borrows[k].mutable,
        });
        k += 1;
    }
    let mut field_borrows = then_desc.field_borrows;
    let mut k = 0;
    while k < else_desc.field_borrows.len() {
        field_borrows.push(FieldHold {
            field: else_desc.field_borrows[k].field.clone(),
            borrows: clone_held_borrows(&else_desc.field_borrows[k].borrows),
        });
        k += 1;
    }
    Ok(ValueDesc { borrows, field_borrows })
}

fn snapshot_clone(
    snap: &Vec<(Vec<HeldBorrow>, Vec<FieldHold>)>,
) -> Vec<(Vec<HeldBorrow>, Vec<FieldHold>)> {
    let mut out: Vec<(Vec<HeldBorrow>, Vec<FieldHold>)> = Vec::new();
    let mut i = 0;
    while i < snap.len() {
        out.push((
            clone_held_borrows(&snap[i].0),
            clone_field_holds(&snap[i].1),
        ));
        i += 1;
    }
    out
}

// Walk a pattern at borrowck time: pushes a Holder for each binding
// (with the binding's resolved RType), records partial moves on
// `state.moved` for non-Copy value bindings rooted at the scrutinee
// place, and seeds `ref` bindings' holders with the borrow they
// represent.
//
// `scrut_path` is `Some(place)` when the scrutinee is a place
// expression (so borrows / partial moves can be rooted at it) or
// `None` for rvalue scrutinees (no place to track). Path extension:
// tuple element `i` becomes path + ["i"]; struct field `f` becomes
// path + ["f"]; variant `V::W(payload)` extends with `["V", "0"]`
// to keep the variant tag distinct from same-named struct fields.
//
// Ref pattern (`&p`) descends into the pointee; pocket-rust doesn't
// give pointees a place identity, so the inner pattern walks with
// `None` — bindings inside still get the right RType, but place-
// rooted move/borrow tracking stops at the deref boundary.
fn walk_pattern_for_borrowck(
    state: &mut BorrowState,
    pattern: &crate::ast::Pattern,
    scrut_ty: &RType,
    scrut_path: Option<&Vec<String>>,
    span: &Span,
) -> Result<(), Error> {
    use crate::ast::PatternKind;
    use crate::typeck::{
        EnumEntry, VariantPayloadResolved, enum_lookup, struct_lookup,
    };
    match &pattern.kind {
        PatternKind::Wildcard
        | PatternKind::LitInt(_)
        | PatternKind::LitBool(_)
        | PatternKind::Range { .. } => Ok(()),
        PatternKind::Binding { name, by_ref, mutable, .. } => {
            if *by_ref {
                // `ref name` / `ref mut name`: holder type is `&T` /
                // `&mut T`; if the scrutinee is a place, the borrow
                // is recorded on the holder so downstream
                // conflict-with-`&mut` checks see it.
                let ref_ty = RType::Ref {
                    inner: Box::new(scrut_ty.clone()),
                    mutable: *mutable,
                    lifetime: crate::typeck::LifetimeRepr::Inferred(0),
                };
                let holds: Vec<HeldBorrow> = match scrut_path {
                    Some(p) => {
                        let new = HeldBorrow {
                            place: p.clone(),
                            mutable: *mutable,
                        };
                        check_borrow_conflict(state, &new, span)?;
                        vec![HeldBorrow {
                            place: p.clone(),
                            mutable: *mutable,
                        }]
                    }
                    None => Vec::new(),
                };
                state.holders.push(Holder {
                    name: Some(name.clone()),
                    rtype: Some(ref_ty),
                    holds,
                    field_holds: Vec::new(),
                });
            } else {
                // Value binding: if the scrutinee is a place and the
                // value is non-Copy, record a partial move at the
                // current sub-path — subsequent uses of the
                // scrutinee will detect the conflict via prefix-
                // overlap.
                if let Some(p) = scrut_path {
                    let copy = is_copy_with_bounds(
                        scrut_ty,
                        state.traits,
                        &state.type_params,
                        &state.type_param_bounds,
                    );
                    if !copy {
                        try_move(state, p.clone(), span.copy())?;
                    }
                }
                state.holders.push(Holder {
                    name: Some(name.clone()),
                    rtype: Some(scrut_ty.clone()),
                    holds: Vec::new(),
                    field_holds: Vec::new(),
                });
            }
            Ok(())
        }
        PatternKind::At { name, inner, .. } => {
            // `name @ inner`: bind name (value-binding semantics)
            // and recurse into inner. If the value is non-Copy and
            // we're rooted at a place, the at-binding moves it.
            if let Some(p) = scrut_path {
                let copy = is_copy_with_bounds(
                    scrut_ty,
                    state.traits,
                    &state.type_params,
                    &state.type_param_bounds,
                );
                if !copy {
                    try_move(state, p.clone(), span.copy())?;
                }
            }
            state.holders.push(Holder {
                name: Some(name.clone()),
                rtype: Some(scrut_ty.clone()),
                holds: Vec::new(),
                field_holds: Vec::new(),
            });
            walk_pattern_for_borrowck(state, inner, scrut_ty, scrut_path, span)
        }
        PatternKind::Tuple(elems) => {
            if let RType::Tuple(elem_tys) = scrut_ty {
                let mut i = 0;
                while i < elems.len() && i < elem_tys.len() {
                    let sub_path = scrut_path.map(|p| {
                        let mut np = p.clone();
                        np.push(format!("{}", i));
                        np
                    });
                    walk_pattern_for_borrowck(
                        state,
                        &elems[i],
                        &elem_tys[i],
                        sub_path.as_ref(),
                        span,
                    )?;
                    i += 1;
                }
            }
            Ok(())
        }
        PatternKind::Ref { inner, .. } => {
            // Match descends through the reference into its pointee.
            // Pocket-rust doesn't give pointees a place identity for
            // borrow tracking, so the inner walks without a path.
            // Bindings inside still get the right RType.
            if let RType::Ref { inner: pointee, .. } = scrut_ty {
                walk_pattern_for_borrowck(state, inner, pointee.as_ref(), None, span)
            } else {
                Ok(())
            }
        }
        PatternKind::VariantTuple { path, elems } => {
            if let RType::Enum { path: enum_path, type_args, .. } = scrut_ty {
                let entry: &EnumEntry = match enum_lookup(state.enums, enum_path) {
                    Some(e) => e,
                    None => return Ok(()),
                };
                let variant_name = match path.segments.last() {
                    Some(s) => s.name.clone(),
                    None => return Ok(()),
                };
                let mut v_idx: Option<usize> = None;
                let mut k = 0;
                while k < entry.variants.len() {
                    if entry.variants[k].name == variant_name {
                        v_idx = Some(k);
                        break;
                    }
                    k += 1;
                }
                let v_idx = match v_idx {
                    Some(i) => i,
                    None => return Ok(()),
                };
                let env = enum_type_env(&entry.type_params, type_args);
                if let VariantPayloadResolved::Tuple(payload_tys) =
                    &entry.variants[v_idx].payload
                {
                    let mut i = 0;
                    while i < elems.len() && i < payload_tys.len() {
                        let sub_ty =
                            crate::typeck::substitute_rtype(&payload_tys[i], &env);
                        let sub_path = scrut_path.map(|p| {
                            let mut np = p.clone();
                            np.push(variant_name.clone());
                            np.push(format!("{}", i));
                            np
                        });
                        walk_pattern_for_borrowck(
                            state,
                            &elems[i],
                            &sub_ty,
                            sub_path.as_ref(),
                            span,
                        )?;
                        i += 1;
                    }
                }
            }
            Ok(())
        }
        PatternKind::VariantStruct { path, fields, .. } => {
            match scrut_ty {
                RType::Enum { path: enum_path, type_args, .. } => {
                    let entry = match enum_lookup(state.enums, enum_path) {
                        Some(e) => e,
                        None => return Ok(()),
                    };
                    let variant_name = match path.segments.last() {
                        Some(s) => s.name.clone(),
                        None => return Ok(()),
                    };
                    let mut v_idx: Option<usize> = None;
                    let mut k = 0;
                    while k < entry.variants.len() {
                        if entry.variants[k].name == variant_name {
                            v_idx = Some(k);
                            break;
                        }
                        k += 1;
                    }
                    let v_idx = match v_idx {
                        Some(i) => i,
                        None => return Ok(()),
                    };
                    let env = enum_type_env(&entry.type_params, type_args);
                    if let VariantPayloadResolved::Struct(field_defs) =
                        &entry.variants[v_idx].payload
                    {
                        let mut k = 0;
                        while k < fields.len() {
                            let mut idx: Option<usize> = None;
                            let mut j = 0;
                            while j < field_defs.len() {
                                if field_defs[j].name == fields[k].name {
                                    idx = Some(j);
                                    break;
                                }
                                j += 1;
                            }
                            if let Some(j) = idx {
                                let sub_ty = crate::typeck::substitute_rtype(
                                    &field_defs[j].ty,
                                    &env,
                                );
                                let sub_path = scrut_path.map(|p| {
                                    let mut np = p.clone();
                                    np.push(variant_name.clone());
                                    np.push(fields[k].name.clone());
                                    np
                                });
                                walk_pattern_for_borrowck(
                                    state,
                                    &fields[k].pattern,
                                    &sub_ty,
                                    sub_path.as_ref(),
                                    span,
                                )?;
                            }
                            k += 1;
                        }
                    }
                }
                RType::Struct { path: struct_path, type_args, .. } => {
                    let entry = match struct_lookup(state.structs, struct_path) {
                        Some(e) => e,
                        None => return Ok(()),
                    };
                    let env = enum_type_env(&entry.type_params, type_args);
                    let mut k = 0;
                    while k < fields.len() {
                        let mut idx: Option<usize> = None;
                        let mut j = 0;
                        while j < entry.fields.len() {
                            if entry.fields[j].name == fields[k].name {
                                idx = Some(j);
                                break;
                            }
                            j += 1;
                        }
                        if let Some(j) = idx {
                            let sub_ty =
                                crate::typeck::substitute_rtype(&entry.fields[j].ty, &env);
                            let sub_path = scrut_path.map(|p| {
                                let mut np = p.clone();
                                np.push(fields[k].name.clone());
                                np
                            });
                            walk_pattern_for_borrowck(
                                state,
                                &fields[k].pattern,
                                &sub_ty,
                                sub_path.as_ref(),
                                span,
                            )?;
                        }
                        k += 1;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        PatternKind::Or(alts) => {
            // Or-patterns: typeck enforces that all alternatives bind
            // the same names with unifiable types. For move/borrow
            // tracking we conservatively use the *union* — if any
            // alternative would move a non-Copy value or hold a
            // borrow, treat the binding as if it always does. The
            // simplest implementation is to walk just the first alt
            // and emit those bindings; alternatives that differ
            // structurally (e.g. `Some(ref x) | None` — illegal in
            // typeck since None doesn't bind x) wouldn't get past
            // typeck anyway. For E3 we keep the first-alt approach.
            if !alts.is_empty() {
                walk_pattern_for_borrowck(state, &alts[0], scrut_ty, scrut_path, span)?;
            }
            Ok(())
        }
    }
}

// Old typed-walk variant (no place-rooted tracking) — kept for
// reference; not used now that walk_pattern_for_borrowck does the
// full job.
#[allow(dead_code)]
fn collect_pattern_bindings_typed(
    pattern: &crate::ast::Pattern,
    scrut_ty: &RType,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    out: &mut Vec<(String, RType)>,
) {
    use crate::ast::PatternKind;
    use crate::typeck::{
        EnumEntry, VariantPayloadResolved, enum_lookup, struct_lookup,
    };
    match &pattern.kind {
        PatternKind::Wildcard
        | PatternKind::LitInt(_)
        | PatternKind::LitBool(_)
        | PatternKind::Range { .. } => {}
        PatternKind::Binding { name, by_ref, mutable, .. } => {
            let ty = if *by_ref {
                RType::Ref {
                    inner: Box::new(scrut_ty.clone()),
                    mutable: *mutable,
                    lifetime: crate::typeck::LifetimeRepr::Inferred(0),
                }
            } else {
                scrut_ty.clone()
            };
            out.push((name.clone(), ty));
        }
        PatternKind::At { name, inner, .. } => {
            out.push((name.clone(), scrut_ty.clone()));
            collect_pattern_bindings_typed(inner, scrut_ty, structs, enums, out);
        }
        PatternKind::Tuple(elems) => {
            if let RType::Tuple(elem_tys) = scrut_ty {
                let mut i = 0;
                while i < elems.len() && i < elem_tys.len() {
                    collect_pattern_bindings_typed(&elems[i], &elem_tys[i], structs, enums, out);
                    i += 1;
                }
            }
        }
        PatternKind::Ref { inner, .. } => {
            if let RType::Ref { inner: pointee, .. } = scrut_ty {
                collect_pattern_bindings_typed(inner, pointee.as_ref(), structs, enums, out);
            }
        }
        PatternKind::VariantTuple { path, elems } => {
            // Look up the variant in the enum corresponding to the
            // scrutinee type, find its tuple-payload types
            // substituted under the scrutinee's type-args.
            if let RType::Enum { path: enum_path, type_args, .. } = scrut_ty {
                let entry: &EnumEntry = match enum_lookup(enums, enum_path) {
                    Some(e) => e,
                    None => return,
                };
                let variant_name = match path.segments.last() {
                    Some(s) => s.name.clone(),
                    None => return,
                };
                let mut variant_idx: Option<usize> = None;
                let mut k = 0;
                while k < entry.variants.len() {
                    if entry.variants[k].name == variant_name {
                        variant_idx = Some(k);
                        break;
                    }
                    k += 1;
                }
                let v_idx = match variant_idx {
                    Some(i) => i,
                    None => return,
                };
                let env = enum_type_env(&entry.type_params, type_args);
                if let VariantPayloadResolved::Tuple(payload_tys) = &entry.variants[v_idx].payload {
                    let mut i = 0;
                    while i < elems.len() && i < payload_tys.len() {
                        let sub_ty =
                            crate::typeck::substitute_rtype(&payload_tys[i], &env);
                        collect_pattern_bindings_typed(&elems[i], &sub_ty, structs, enums, out);
                        i += 1;
                    }
                }
            }
        }
        PatternKind::VariantStruct { path, fields, .. } => {
            // Two cases: the path resolves to an enum variant (struct-
            // shaped payload) OR to a bare struct type. Distinguish
            // by the scrutinee's resolved type.
            match scrut_ty {
                RType::Enum { path: enum_path, type_args, .. } => {
                    let entry = match enum_lookup(enums, enum_path) {
                        Some(e) => e,
                        None => return,
                    };
                    let variant_name = match path.segments.last() {
                        Some(s) => s.name.clone(),
                        None => return,
                    };
                    let mut v_idx: Option<usize> = None;
                    let mut k = 0;
                    while k < entry.variants.len() {
                        if entry.variants[k].name == variant_name {
                            v_idx = Some(k);
                            break;
                        }
                        k += 1;
                    }
                    let v_idx = match v_idx {
                        Some(i) => i,
                        None => return,
                    };
                    let env = enum_type_env(&entry.type_params, type_args);
                    if let VariantPayloadResolved::Struct(field_defs) =
                        &entry.variants[v_idx].payload
                    {
                        let mut k = 0;
                        while k < fields.len() {
                            let mut idx: Option<usize> = None;
                            let mut j = 0;
                            while j < field_defs.len() {
                                if field_defs[j].name == fields[k].name {
                                    idx = Some(j);
                                    break;
                                }
                                j += 1;
                            }
                            if let Some(j) = idx {
                                let sub_ty = crate::typeck::substitute_rtype(
                                    &field_defs[j].ty,
                                    &env,
                                );
                                collect_pattern_bindings_typed(
                                    &fields[k].pattern,
                                    &sub_ty,
                                    structs,
                                    enums,
                                    out,
                                );
                            }
                            k += 1;
                        }
                    }
                }
                RType::Struct { path: struct_path, type_args, .. } => {
                    let entry = match struct_lookup(structs, struct_path) {
                        Some(e) => e,
                        None => return,
                    };
                    let env = enum_type_env(&entry.type_params, type_args);
                    let mut k = 0;
                    while k < fields.len() {
                        let mut idx: Option<usize> = None;
                        let mut j = 0;
                        while j < entry.fields.len() {
                            if entry.fields[j].name == fields[k].name {
                                idx = Some(j);
                                break;
                            }
                            j += 1;
                        }
                        if let Some(j) = idx {
                            let sub_ty =
                                crate::typeck::substitute_rtype(&entry.fields[j].ty, &env);
                            collect_pattern_bindings_typed(
                                &fields[k].pattern,
                                &sub_ty,
                                structs,
                                enums,
                                out,
                            );
                        }
                        k += 1;
                    }
                }
                _ => {}
            }
        }
        PatternKind::Or(alts) => {
            // All alts bind the same set with unifiable types
            // (typeck-enforced); use the first alt to seed.
            if !alts.is_empty() {
                collect_pattern_bindings_typed(&alts[0], scrut_ty, structs, enums, out);
            }
        }
    }
}

// Build a (param_name, concrete_type) env from a type's declared
// type-params and concrete type-args. Used to substitute Param slots
// in payload/field types during pattern-type recursion.
fn enum_type_env(params: &Vec<String>, args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let n = if params.len() < args.len() { params.len() } else { args.len() };
    let mut i = 0;
    while i < n {
        env.push((params[i].clone(), args[i].clone()));
        i += 1;
    }
    env
}

// Walk a pattern recursively and append every binding name it
// introduces (Ident, At, VariantTuple/Struct elements/fields, Tuple
// elements, Ref inner, Or alternatives).
fn collect_pattern_bindings(pattern: &crate::ast::Pattern, out: &mut Vec<String>) {
    use crate::ast::PatternKind;
    match &pattern.kind {
        PatternKind::Wildcard | PatternKind::LitInt(_) | PatternKind::LitBool(_) => {}
        PatternKind::Binding { name, .. } => out.push(name.clone()),
        PatternKind::At { name, inner, .. } => {
            out.push(name.clone());
            collect_pattern_bindings(inner, out);
        }
        PatternKind::VariantTuple { elems, .. } => {
            let mut k = 0;
            while k < elems.len() {
                collect_pattern_bindings(&elems[k], out);
                k += 1;
            }
        }
        PatternKind::VariantStruct { fields, .. } => {
            let mut k = 0;
            while k < fields.len() {
                collect_pattern_bindings(&fields[k].pattern, out);
                k += 1;
            }
        }
        PatternKind::Tuple(elems) => {
            let mut k = 0;
            while k < elems.len() {
                collect_pattern_bindings(&elems[k], out);
                k += 1;
            }
        }
        PatternKind::Ref { inner, .. } => collect_pattern_bindings(inner, out),
        PatternKind::Or(alts) => {
            // All alts bind the same set; pick the first.
            if !alts.is_empty() {
                collect_pattern_bindings(&alts[0], out);
            }
        }
        PatternKind::Range { .. } => {}
    }
}

// `if cond { … } else { … }` — walk the cond first (its borrows die
// when the cond expression returns, since exactly one arm runs next).
// Then walk the two arms with the same pre-state, snapshotting and
// restoring around each. Merge the post-states: a place moved in
// both arms stays Moved; a place moved in only one becomes MaybeMoved
// (codegen turns those into flagged drops). The result desc unions
// the two arms' tail borrows.
fn walk_if_expr(
    state: &mut BorrowState,
    if_expr: &crate::ast::IfExpr,
) -> Result<ValueDesc, Error> {
    // Walk the cond as a regular sub-expression (its borrows go into a
    // synthetic call slot that pops at the end of cond evaluation —
    // matching how a function call evaluates its arg list).
    let cond_call_slot = state.holders.len();
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let cond_desc = walk_expr(state, &if_expr.cond)?;
    let new = HeldBorrow_vec_from_desc(&cond_desc);
    state.holders[cond_call_slot].holds = new;
    state.holders.truncate(cond_call_slot);

    // Snapshot the pre-arm state so we can replay the second arm cleanly.
    let pre_moved: Vec<MovedPlace> = clone_moved_vec(&state.moved);
    let pre_holders_len = state.holders.len();
    let pre_holders_state: Vec<(Vec<HeldBorrow>, Vec<FieldHold>)> = snapshot_holders_state(state);

    // Walk arm 1.
    let arm1_desc = walk_block_expr(state, if_expr.then_block.as_ref())?;
    let arm1_moved = clone_moved_vec(&state.moved);

    // Restore pre-state and walk arm 2.
    state.moved = pre_moved;
    state.holders.truncate(pre_holders_len);
    restore_holders_state(state, pre_holders_state);
    let arm2_desc = walk_block_expr(state, if_expr.else_block.as_ref())?;
    let arm2_moved = clone_moved_vec(&state.moved);

    // Merge arm1 and arm2 post-states. Any place moved in both arms
    // stays Moved (the post-state is `Moved` regardless of branch). A
    // place in only one arm becomes MaybeMoved.
    state.moved = merge_moved_sets(&arm1_moved, &arm2_moved);

    // Holders that escaped both arms via their tail descs survived; the
    // arm-internal holders are already gone from `state.holders` since
    // walk_block_expr pops back to its mark. Now the if's value is the
    // union of both arms' tail borrows. Caller (let / call slot / etc.)
    // decides where they land.
    let mut borrows = arm1_desc.borrows;
    let mut k = 0;
    while k < arm2_desc.borrows.len() {
        borrows.push(HeldBorrow {
            place: arm2_desc.borrows[k].place.clone(),
            mutable: arm2_desc.borrows[k].mutable,
        });
        k += 1;
    }
    let mut field_borrows = arm1_desc.field_borrows;
    let mut k = 0;
    while k < arm2_desc.field_borrows.len() {
        field_borrows.push(FieldHold {
            field: arm2_desc.field_borrows[k].field.clone(),
            borrows: clone_held_borrows(&arm2_desc.field_borrows[k].borrows),
        });
        k += 1;
    }
    Ok(ValueDesc { borrows, field_borrows })
}

fn HeldBorrow_vec_from_desc(desc: &ValueDesc) -> Vec<HeldBorrow> {
    clone_held_borrows(&desc.borrows)
}

fn clone_moved_vec(v: &Vec<MovedPlace>) -> Vec<MovedPlace> {
    let mut out: Vec<MovedPlace> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(v[i].clone());
        i += 1;
    }
    out
}

fn snapshot_holders_state(state: &BorrowState) -> Vec<(Vec<HeldBorrow>, Vec<FieldHold>)> {
    let mut out: Vec<(Vec<HeldBorrow>, Vec<FieldHold>)> = Vec::new();
    let mut i = 0;
    while i < state.holders.len() {
        out.push((
            clone_held_borrows(&state.holders[i].holds),
            clone_field_holds(&state.holders[i].field_holds),
        ));
        i += 1;
    }
    out
}

fn restore_holders_state(
    state: &mut BorrowState,
    saved: Vec<(Vec<HeldBorrow>, Vec<FieldHold>)>,
) {
    let mut i = 0;
    while i < saved.len() && i < state.holders.len() {
        state.holders[i].holds = saved[i].0.iter().map(|b| HeldBorrow {
            place: b.place.clone(),
            mutable: b.mutable,
        }).collect();
        state.holders[i].field_holds = clone_field_holds(&saved[i].1);
        i += 1;
    }
}

// Merge two post-arm move sets. A place is `Moved` in the merge iff it
// was Moved in both arms; `MaybeMoved` if it's in exactly one (or is
// MaybeMoved in either and present in the other in some form). This is
// the join point for control-flow.
fn merge_moved_sets(a: &Vec<MovedPlace>, b: &Vec<MovedPlace>) -> Vec<MovedPlace> {
    let mut out: Vec<MovedPlace> = Vec::new();
    let mut i = 0;
    while i < a.len() {
        let in_b = find_place(b, &a[i].place);
        let merged_status = match (&a[i].status, in_b) {
            (MoveStatus::Moved, Some(MoveStatus::Moved)) => MoveStatus::Moved,
            _ => MoveStatus::MaybeMoved,
        };
        out.push(MovedPlace {
            place: a[i].place.clone(),
            status: merged_status,
        });
        i += 1;
    }
    let mut j = 0;
    while j < b.len() {
        if find_place(a, &b[j].place).is_none() {
            // Only in b → MaybeMoved (a's path is implicitly Init).
            out.push(MovedPlace {
                place: b[j].place.clone(),
                status: MoveStatus::MaybeMoved,
            });
        }
        j += 1;
    }
    out
}

fn find_place(set: &Vec<MovedPlace>, place: &Vec<String>) -> Option<MoveStatus> {
    let mut i = 0;
    while i < set.len() {
        if &set[i].place == place {
            return Some(set[i].status.clone());
        }
        i += 1;
    }
    None
}

fn walk_method_call(
    state: &mut BorrowState,
    mc: &MethodCall,
    node_id: crate::ast::NodeId,
) -> Result<ValueDesc, Error> {
    let res = state.method_resolutions[node_id as usize]
        .as_ref()
        .expect("typeck registered this method call");
    let recv_adjust = match &res.recv_adjust {
        ReceiverAdjust::Move => RecvAdjustLocal::Move,
        ReceiverAdjust::BorrowImm => RecvAdjustLocal::BorrowImm,
        ReceiverAdjust::BorrowMut => RecvAdjustLocal::BorrowMut,
        ReceiverAdjust::ByRef => RecvAdjustLocal::ByRef,
    };
    let ret_borrows_recv = res.ret_borrows_receiver;
    // Push synthetic call slot.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let call_idx = state.holders.len() - 1;
    // Process the receiver per recv_adjust.
    let recv_borrows: Vec<HeldBorrow> = match recv_adjust {
        RecvAdjustLocal::Move => {
            // Treat recv as an arg — walk it for moves, absorb borrows.
            let desc = walk_expr(state, &mc.receiver)?;
            let snapshot = clone_held_borrows(&desc.borrows);
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: desc.borrows[k].place.clone(),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &mc.receiver.span)?;
                state.holders[call_idx].holds.push(new);
                k += 1;
            }
            snapshot
        }
        RecvAdjustLocal::BorrowImm | RecvAdjustLocal::BorrowMut => {
            // Synthesize a borrow on recv (recv must be a place expr; typeck verified).
            let mutable = matches!(recv_adjust, RecvAdjustLocal::BorrowMut);
            walk_synth_borrow(state, &mc.receiver, mutable, call_idx)?
        }
        RecvAdjustLocal::ByRef => {
            // Recv is already a ref — walk as a regular var read; its borrows
            // get absorbed into the call slot (and snapshotted for propagation).
            let desc = walk_expr(state, &mc.receiver)?;
            let snapshot = clone_held_borrows(&desc.borrows);
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: desc.borrows[k].place.clone(),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &mc.receiver.span)?;
                state.holders[call_idx].holds.push(new);
                k += 1;
            }
            snapshot
        }
    };
    // Process remaining args. Per-slot field_borrows from struct args are
    // flattened into the call slot alongside direct borrows.
    let mut i = 0;
    while i < mc.args.len() {
        let desc = walk_expr(state, &mc.args[i])?;
        let mut k = 0;
        while k < desc.borrows.len() {
            let new = HeldBorrow {
                place: desc.borrows[k].place.clone(),
                mutable: desc.borrows[k].mutable,
            };
            check_borrow_conflict(state, &new, &mc.args[i].span)?;
            state.holders[call_idx].holds.push(new);
            k += 1;
        }
        let mut f = 0;
        while f < desc.field_borrows.len() {
            let mut k = 0;
            while k < desc.field_borrows[f].borrows.len() {
                let new = HeldBorrow {
                    place: desc.field_borrows[f].borrows[k].place.clone(),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &mc.args[i].span)?;
                state.holders[call_idx].holds.push(new);
                k += 1;
            }
            f += 1;
        }
        i += 1;
    }
    state.holders.truncate(call_idx);
    if ret_borrows_recv {
        Ok(ValueDesc {
            borrows: recv_borrows,
            field_borrows: Vec::new(),
        })
    } else {
        Ok(empty_desc())
    }
}

enum RecvAdjustLocal {
    Move,
    BorrowImm,
    BorrowMut,
    ByRef,
}

// Synthesize a `&recv` (or `&mut recv`) borrow, with the same conflict checks
// `walk_borrow` would apply, and absorb the result into the call slot.
fn walk_synth_borrow(
    state: &mut BorrowState,
    inner: &Expr,
    mutable: bool,
    call_idx: usize,
) -> Result<Vec<HeldBorrow>, Error> {
    let place = match extract_place(inner) {
        Some(p) => p,
        None => {
            // Non-place receiver — autoref of a temporary. Walk for side
            // effects; produces no borrow.
            walk_expr(state, inner)?;
            return Ok(Vec::new());
        }
    };
    // Check it hasn't been moved (or maybe-moved on some path).
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i].place, &place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!(
                    "cannot borrow `{}`: it has been moved",
                    place_to_string(&place)
                ),
                span: inner.span.copy(),
            });
        }
        i += 1;
    }
    let new = HeldBorrow {
        place: place.clone(),
        mutable,
    };
    check_borrow_conflict(state, &new, &inner.span)?;
    state.holders[call_idx].holds.push(new);
    let mut snapshot: Vec<HeldBorrow> = Vec::new();
    snapshot.push(HeldBorrow { place, mutable });
    Ok(snapshot)
}

fn walk_var(state: &mut BorrowState, name: &str, expr: &Expr) -> Result<ValueDesc, Error> {
    let idx = find_binding(state, name).expect("typeck verified the variable exists");
    if is_raw_ptr_holder(&state.holders[idx]) {
        // Raw pointers are Copy and carry no borrow handles.
        return Ok(empty_desc());
    }
    if is_ref_holder(&state.holders[idx]) {
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        check_not_moved(state, &place, &expr.span)?;
        if is_mut_ref_holder(&state.holders[idx]) {
            // `&mut T` is not really Copy under our borrow model — we don't
            // implement implicit reborrow, so reading a `&mut` binding moves
            // its borrow into the consumer (call slot or new binding) and the
            // binding becomes unusable afterward. Liveness GC alone isn't
            // sufficient because both the source binding and the consumer
            // would otherwise hold the same exclusive borrow during arg
            // evaluation.
            let mut taken: Vec<HeldBorrow> = Vec::new();
            std::mem::swap(&mut taken, &mut state.holders[idx].holds);
            state.moved.push(MovedPlace { place, status: MoveStatus::Moved });
            Ok(ValueDesc { borrows: taken, field_borrows: Vec::new() })
        } else {
            // `&T` is Copy: cloning the borrow handle is fine.
            let holds = clone_held_borrows(&state.holders[idx].holds);
            Ok(ValueDesc { borrows: holds, field_borrows: Vec::new() })
        }
    } else if is_owned_copy_holder(
        &state.holders[idx],
        state.traits,
        &state.type_params,
        &state.type_param_bounds,
    ) {
        // Owned Copy primitive (ints, etc.): reading is a value copy, no move,
        // no borrows to forward. Still must refuse reads from a moved place.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        check_not_moved(state, &place, &expr.span)?;
        Ok(empty_desc())
    } else {
        // Owned non-Copy (struct): tracked as a move. If the holder has
        // per-slot field_holds (Phase D: struct with ref fields), transfer
        // them into the consumer's desc so the new binding/call slot keeps
        // those borrows alive.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        try_move(state, place, expr.span.copy())?;
        // Record the move site so codegen can clear the drop flag.
        state.move_sites.push((expr.id, name.to_string()));
        let mut taken: Vec<FieldHold> = Vec::new();
        std::mem::swap(&mut taken, &mut state.holders[idx].field_holds);
        Ok(ValueDesc {
            borrows: Vec::new(),
            field_borrows: taken,
        })
    }
}

fn walk_call(
    state: &mut BorrowState,
    call: &Call,
    node_id: crate::ast::NodeId,
) -> Result<ValueDesc, Error> {
    // Phase D: borrow propagation through ref-returning calls flows along
    // lifetimes. Look up the callee's `ret_lifetime`; collect every param
    // whose outermost lifetime matches — those args' borrows all propagate
    // into the result (combined borrow sets when one lifetime ties to
    // multiple args).
    let ret_ref_sources: Vec<usize> = match state.call_resolutions[node_id as usize]
        .as_ref()
        .expect("typeck registered this call")
    {
        CallResolution::Direct(idx) => {
            let entry = &state.funcs.entries[*idx];
            match &entry.ret_lifetime {
                Some(rl) => find_lifetime_source(&entry.param_lifetimes, rl),
                None => Vec::new(),
            }
        }
        CallResolution::Generic { template_idx, .. } => {
            let t = &state.funcs.templates[*template_idx];
            match &t.ret_lifetime {
                Some(rl) => find_lifetime_source(&t.param_lifetimes, rl),
                None => Vec::new(),
            }
        }
        // Variant construction yields a fresh enum value at a new
        // address — no input refs flow through into the result.
        CallResolution::Variant { .. } => Vec::new(),
    };

    // Push a synthetic call holder. Borrows produced by argument expressions
    // become its holds for the duration of the call, then the holder is popped.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let call_idx = state.holders.len() - 1;
    // Snapshot each arg's borrows (including any per-slot field borrows
    // flattened together) before they're absorbed into the call slot, so
    // we can later attach the source arg's borrows to the result desc.
    let mut arg_borrow_snapshots: Vec<Vec<HeldBorrow>> = Vec::new();
    let mut i = 0;
    while i < call.args.len() {
        let desc = walk_expr(state, &call.args[i])?;
        // Combine direct + per-slot borrows into one flat snapshot.
        let mut combined: Vec<HeldBorrow> = clone_held_borrows(&desc.borrows);
        let mut f = 0;
        while f < desc.field_borrows.len() {
            let mut k = 0;
            while k < desc.field_borrows[f].borrows.len() {
                combined.push(HeldBorrow {
                    place: desc.field_borrows[f].borrows[k].place.clone(),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                });
                k += 1;
            }
            f += 1;
        }
        arg_borrow_snapshots.push(clone_held_borrows(&combined));
        let mut k = 0;
        while k < combined.len() {
            // Conflict-check the new borrow against every other holder's holds.
            let new = HeldBorrow {
                place: combined[k].place.clone(),
                mutable: combined[k].mutable,
            };
            check_borrow_conflict(state, &new, &call.args[i].span)?;
            state.holders[call_idx].holds.push(new);
            k += 1;
        }
        i += 1;
    }
    state.holders.truncate(call_idx);
    if ret_ref_sources.is_empty() {
        return Ok(empty_desc());
    }
    // Combine borrow sets from every matching arg slot.
    let mut combined: Vec<HeldBorrow> = Vec::new();
    let mut s = 0;
    while s < ret_ref_sources.len() {
        let idx = ret_ref_sources[s];
        let mut k = 0;
        while k < arg_borrow_snapshots[idx].len() {
            combined.push(HeldBorrow {
                place: arg_borrow_snapshots[idx][k].place.clone(),
                mutable: arg_borrow_snapshots[idx][k].mutable,
            });
            k += 1;
        }
        s += 1;
    }
    Ok(ValueDesc {
        borrows: combined,
        field_borrows: Vec::new(),
    })
}

fn check_borrow_conflict(
    state: &BorrowState,
    new: &HeldBorrow,
    span: &Span,
) -> Result<(), Error> {
    let mut h = 0;
    while h < state.holders.len() {
        let mut k = 0;
        while k < state.holders[h].holds.len() {
            let other = &state.holders[h].holds[k];
            if paths_share_prefix(&other.place, &new.place)
                && (other.mutable || new.mutable)
            {
                let kind = if new.mutable { "mutable" } else { "shared" };
                let other_kind = if other.mutable { "mutable" } else { "shared" };
                return Err(Error {
                    file: state.file.clone(),
                    message: format!(
                        "cannot borrow `{}` as {}: already borrowed as {}",
                        place_to_string(&new.place),
                        kind,
                        other_kind
                    ),
                    span: span.copy(),
                });
            }
            k += 1;
        }
        // Phase D: per-slot field_holds also count as live borrows.
        let mut f = 0;
        while f < state.holders[h].field_holds.len() {
            let mut k = 0;
            while k < state.holders[h].field_holds[f].borrows.len() {
                let other = &state.holders[h].field_holds[f].borrows[k];
                if paths_share_prefix(&other.place, &new.place)
                    && (other.mutable || new.mutable)
                {
                    let kind = if new.mutable { "mutable" } else { "shared" };
                    let other_kind = if other.mutable { "mutable" } else { "shared" };
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot borrow `{}` as {}: already borrowed as {}",
                            place_to_string(&new.place),
                            kind,
                            other_kind
                        ),
                        span: span.copy(),
                    });
                }
                k += 1;
            }
            f += 1;
        }
        h += 1;
    }
    Ok(())
}

fn walk_struct_lit(state: &mut BorrowState, lit: &StructLit) -> Result<ValueDesc, Error> {
    // Phase D: a struct field may be a ref. Each field initializer's borrows
    // get tagged with the field name and propagated as `field_borrows` of
    // the resulting value, so a binding holder can keep per-slot tracking.
    // While walking field initializers we push a synthetic holder so any
    // in-flight borrows from earlier fields are visible to conflict checks
    // in later fields' initializers.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let synth_idx = state.holders.len() - 1;
    let mut field_borrows: Vec<FieldHold> = Vec::new();
    let mut i = 0;
    while i < lit.fields.len() {
        let desc = walk_expr(state, &lit.fields[i].value)?;
        if !desc.borrows.is_empty() {
            // Tag this slot's borrows. Also register them in the synthetic
            // holder so subsequent fields' borrows see the conflict.
            let mut grouped: Vec<HeldBorrow> = Vec::new();
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: desc.borrows[k].place.clone(),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &lit.fields[i].value.span)?;
                state.holders[synth_idx].holds.push(new);
                grouped.push(HeldBorrow {
                    place: desc.borrows[k].place.clone(),
                    mutable: desc.borrows[k].mutable,
                });
                k += 1;
            }
            field_borrows.push(FieldHold {
                field: vec![lit.fields[i].name.clone()],
                borrows: grouped,
            });
        }
        // Nested per-slot: when the field's initializer is itself a struct
        // value carrying its own field_borrows, prepend the current field
        // name to each entry's path so the outer holder can find the borrow
        // through `outer.this_field.<inner...>`.
        let mut f = 0;
        while f < desc.field_borrows.len() {
            let mut grouped: Vec<HeldBorrow> = Vec::new();
            let mut k = 0;
            while k < desc.field_borrows[f].borrows.len() {
                let new = HeldBorrow {
                    place: desc.field_borrows[f].borrows[k].place.clone(),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &lit.fields[i].value.span)?;
                state.holders[synth_idx].holds.push(new);
                grouped.push(HeldBorrow {
                    place: desc.field_borrows[f].borrows[k].place.clone(),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                });
                k += 1;
            }
            let mut nested: Vec<String> = Vec::new();
            nested.push(lit.fields[i].name.clone());
            let mut s = 0;
            while s < desc.field_borrows[f].field.len() {
                nested.push(desc.field_borrows[f].field[s].clone());
                s += 1;
            }
            field_borrows.push(FieldHold {
                field: nested,
                borrows: grouped,
            });
            f += 1;
        }
        i += 1;
    }
    state.holders.truncate(synth_idx);
    Ok(ValueDesc {
        borrows: Vec::new(),
        field_borrows,
    })
}

// `(a, b, c)` — analogous to a struct literal. Each element gets a
// positional field name `"0"`, `"1"`, …, so per-slot borrow tracking
// uses the same `field_holds` machinery as structs (a `&T`-typed
// element produces a FieldHold at path `["0"]`, etc.).
fn walk_tuple(state: &mut BorrowState, elems: &Vec<Expr>) -> Result<ValueDesc, Error> {
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
        field_holds: Vec::new(),
    });
    let synth_idx = state.holders.len() - 1;
    let mut field_borrows: Vec<FieldHold> = Vec::new();
    let mut i = 0;
    while i < elems.len() {
        let desc = walk_expr(state, &elems[i])?;
        let name = format!("{}", i);
        if !desc.borrows.is_empty() {
            let mut grouped: Vec<HeldBorrow> = Vec::new();
            let mut k = 0;
            while k < desc.borrows.len() {
                let new = HeldBorrow {
                    place: desc.borrows[k].place.clone(),
                    mutable: desc.borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &elems[i].span)?;
                state.holders[synth_idx].holds.push(new);
                grouped.push(HeldBorrow {
                    place: desc.borrows[k].place.clone(),
                    mutable: desc.borrows[k].mutable,
                });
                k += 1;
            }
            field_borrows.push(FieldHold {
                field: vec![name.clone()],
                borrows: grouped,
            });
        }
        let mut f = 0;
        while f < desc.field_borrows.len() {
            let mut grouped: Vec<HeldBorrow> = Vec::new();
            let mut k = 0;
            while k < desc.field_borrows[f].borrows.len() {
                let new = HeldBorrow {
                    place: desc.field_borrows[f].borrows[k].place.clone(),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                };
                check_borrow_conflict(state, &new, &elems[i].span)?;
                state.holders[synth_idx].holds.push(new);
                grouped.push(HeldBorrow {
                    place: desc.field_borrows[f].borrows[k].place.clone(),
                    mutable: desc.field_borrows[f].borrows[k].mutable,
                });
                k += 1;
            }
            let mut nested: Vec<String> = Vec::new();
            nested.push(name.clone());
            let mut s = 0;
            while s < desc.field_borrows[f].field.len() {
                nested.push(desc.field_borrows[f].field[s].clone());
                s += 1;
            }
            field_borrows.push(FieldHold {
                field: nested,
                borrows: grouped,
            });
            f += 1;
        }
        i += 1;
    }
    state.holders.truncate(synth_idx);
    Ok(ValueDesc {
        borrows: Vec::new(),
        field_borrows,
    })
}

// `t.<index>` — tuple-position analog of `walk_field_access`. Behaves
// identically except we use the numeric index (as a string) as the
// segment name when consulting per-slot field_holds.
fn walk_tuple_index(
    state: &mut BorrowState,
    base: &Expr,
    index: u32,
    expr: &Expr,
) -> Result<ValueDesc, Error> {
    match extract_place(expr) {
        Some(place) => {
            let root_idx =
                find_binding(state, &place[0]).expect("typeck verified the variable exists");
            if is_ref_holder(&state.holders[root_idx]) {
                Ok(empty_desc())
            } else {
                let elem_ty = state.expr_types[expr.id as usize].clone();
                let elem_is_ref = matches!(&elem_ty, Some(RType::Ref { .. }));
                let elem_is_copy = elem_ty
                    .as_ref()
                    .map(|t| {
                        is_copy_with_bounds(
                            t,
                            state.traits,
                            &state.type_params,
                            &state.type_param_bounds,
                        )
                    })
                    .unwrap_or(false);
                check_not_moved(state, &place, &expr.span)?;
                if elem_is_ref && place.len() >= 2 {
                    let sub = &place[1..];
                    let mut found_borrows: Vec<HeldBorrow> = Vec::new();
                    let mut k = 0;
                    while k < state.holders[root_idx].field_holds.len() {
                        if field_path_matches(&state.holders[root_idx].field_holds[k].field, sub) {
                            found_borrows = clone_held_borrows(
                                &state.holders[root_idx].field_holds[k].borrows,
                            );
                            break;
                        }
                        k += 1;
                    }
                    return Ok(ValueDesc {
                        borrows: found_borrows,
                        field_borrows: Vec::new(),
                    });
                }
                if elem_is_copy {
                    // already checked not-moved above
                } else {
                    try_move(state, place, expr.span.copy())?;
                }
                Ok(empty_desc())
            }
        }
        None => {
            walk_expr(state, base)?;
            let _ = index;
            Ok(empty_desc())
        }
    }
}

fn walk_field_access(
    state: &mut BorrowState,
    fa: &FieldAccess,
    expr: &Expr,
) -> Result<ValueDesc, Error> {
    match extract_place(expr) {
        Some(place) => {
            let root_idx =
                find_binding(state, &place[0]).expect("typeck verified the variable exists");
            if is_ref_holder(&state.holders[root_idx]) {
                // Navigation through a reference. Field is Copy (typeck), so
                // the result is plain data with no carried borrows; the ref
                // itself is unchanged.
                Ok(empty_desc())
            } else {
                // Field access on an owned root.
                let field_ty = state.expr_types[expr.id as usize].clone();
                let field_is_ref = matches!(&field_ty, Some(RType::Ref { .. }));
                let field_is_copy = field_ty
                    .as_ref()
                    .map(|t| {
                        is_copy_with_bounds(
                            t,
                            state.traits,
                            &state.type_params,
                            &state.type_param_bounds,
                        )
                    })
                    .unwrap_or(false);
                check_not_moved(state, &place, &expr.span)?;
                // Per-slot lookup: if the leaf is ref-typed, find the
                // FieldHold whose field path matches the chain after the
                // root (e.g. `a.b.r` matches `field == ["b","r"]`) and
                // propagate its borrows. Works for both top-level
                // (`["r"]`) and nested (`["b","r"]`) entries.
                if field_is_ref && place.len() >= 2 {
                    let sub = &place[1..];
                    let mut found_borrows: Vec<HeldBorrow> = Vec::new();
                    let mut k = 0;
                    while k < state.holders[root_idx].field_holds.len() {
                        if field_path_matches(&state.holders[root_idx].field_holds[k].field, sub) {
                            found_borrows = clone_held_borrows(
                                &state.holders[root_idx].field_holds[k].borrows,
                            );
                            break;
                        }
                        k += 1;
                    }
                    return Ok(ValueDesc {
                        borrows: found_borrows,
                        field_borrows: Vec::new(),
                    });
                }
                if field_is_copy {
                    // already checked not-moved above
                } else {
                    try_move(state, place, expr.span.copy())?;
                }
                Ok(empty_desc())
            }
        }
        None => {
            // Field access on a non-place base (e.g. a call result). Walk the
            // base for its side effects; the field result is Copy.
            walk_expr(state, &fa.base)?;
            Ok(empty_desc())
        }
    }
}

fn walk_borrow(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    let (inner, mutable) = match &expr.kind {
        ExprKind::Borrow { inner, mutable } => (inner.as_ref(), *mutable),
        _ => unreachable!("walk_borrow called on non-Borrow"),
    };
    match extract_place(inner) {
        Some(place) => {
            // Refuse to borrow a place whose root has already been moved.
            let mut i = 0;
            while i < state.moved.len() {
                if paths_share_prefix(&state.moved[i].place, &place) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot borrow `{}`: it has been moved",
                            place_to_string(&place)
                        ),
                        span: expr.span.copy(),
                    });
                }
                i += 1;
            }
            let new = HeldBorrow {
                place: place.clone(),
                mutable,
            };
            check_borrow_conflict(state, &new, &expr.span)?;
            let mut borrows = Vec::new();
            borrows.push(HeldBorrow { place, mutable });
            Ok(ValueDesc { borrows, field_borrows: Vec::new() })
        }
        None => {
            // Borrowing a non-place expression (e.g. `&fresh_struct_lit()`).
            // We still walk inner for its move-tracking side effects, but
            // don't track the borrow (no place to point at).
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
    }
}

fn walk_block_expr(state: &mut BorrowState, block: &Block) -> Result<ValueDesc, Error> {
    // The block introduces a fresh local scope. Any holders pushed inside
    // (let bindings) are dropped when the block ends. The block's tail value
    // descriptor is returned to the caller — its borrows survive the scope
    // because the *caller* will turn them into a holder of its own.
    let mark = state.holders.len();
    let desc = walk_stmts_and_tail(state, block)?;
    state.holders.truncate(mark);
    Ok(desc)
}

// ---------- Helpers ----------

fn find_binding(state: &BorrowState, name: &str) -> Option<usize> {
    let mut i = state.holders.len();
    while i > 0 {
        i -= 1;
        if let Some(n) = &state.holders[i].name {
            if n == name {
                return Some(i);
            }
        }
    }
    None
}

fn is_ref_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::Ref { .. }))
}

fn is_raw_ptr_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::RawPtr { .. }))
}

fn is_mut_ref_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::Ref { mutable: true, .. }))
}

// True for owned Copy primitives (ints currently; not refs or raw pointers,
// which are handled by their own dedicated branches in walk_var). Reading
// such a binding produces a value copy — no move, no borrow to forward.
fn is_owned_copy_holder(
    h: &Holder,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bounds: &Vec<Vec<Vec<String>>>,
) -> bool {
    match &h.rtype {
        Some(rt) => is_copy_with_bounds(rt, traits, type_params, type_param_bounds),
        _ => false,
    }
}


fn is_deref_rooted_assign(expr: &Expr) -> bool {
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Deref(_) => return true,
            ExprKind::FieldAccess(fa) => current = &fa.base,
            ExprKind::TupleIndex { base, .. } => current = base,
            _ => return false,
        }
    }
}

// Walk the deref-rooted LHS for its side effects: typically the chain of
// FieldAccess/Deref nodes ends at a Var (the &mut binding being written
// through), and we want to surface that read.
fn walk_assign_lhs(state: &mut BorrowState, expr: &Expr) -> Result<(), Error> {
    match &expr.kind {
        ExprKind::Deref(inner) => {
            walk_expr(state, inner)?;
            Ok(())
        }
        ExprKind::FieldAccess(fa) => walk_assign_lhs(state, &fa.base),
        ExprKind::TupleIndex { base, .. } => walk_assign_lhs(state, base),
        _ => {
            walk_expr(state, expr)?;
            Ok(())
        }
    }
}

fn extract_place(expr: &Expr) -> Option<Vec<String>> {
    let mut chain: Vec<String> = Vec::new();
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Var(name) => {
                chain.push(name.clone());
                let mut reversed: Vec<String> = Vec::new();
                let mut i = chain.len();
                while i > 0 {
                    i -= 1;
                    reversed.push(chain[i].clone());
                }
                return Some(reversed);
            }
            ExprKind::FieldAccess(fa) => {
                chain.push(fa.field.clone());
                current = &fa.base;
            }
            ExprKind::TupleIndex { base, index, .. } => {
                chain.push(format!("{}", index));
                current = base;
            }
            _ => return None,
        }
    }
}

// Check that a place hasn't already been moved out of. Used for Copy reads
// (which don't add to the moved set but still must refuse to read from a
// moved place).
fn check_not_moved(
    state: &BorrowState,
    place: &Vec<String>,
    span: &Span,
) -> Result<(), Error> {
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i].place, place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!("`{}` was already moved", place_to_string(place)),
                span: span.copy(),
            });
        }
        i += 1;
    }
    Ok(())
}

fn try_move(state: &mut BorrowState, place: Vec<String>, span: Span) -> Result<(), Error> {
    // T4.6: whole-binding moves of Drop types are allowed; codegen consults
    // the moved set and skips the implicit scope-end drop for moved
    // bindings, so only the final owner drops. Partial moves out of a Drop
    // value are still rejected — Drop's destructor runs over the whole
    // value, so leaving a hole would mean either dropping a partially-
    // initialized value or silently leaking the still-live fields.
    if place.len() > 1 {
        if let Some(idx) = find_binding(state, &place[0]) {
            if let Some(rt) = &state.holders[idx].rtype {
                if crate::typeck::is_drop(rt, state.traits) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot move out of `{}`: type implements `Drop`",
                            place_to_string(&place)
                        ),
                        span,
                    });
                }
            }
        }
    }
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i].place, &place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!("`{}` was already moved", place_to_string(&place)),
                span,
            });
        }
        i += 1;
    }
    let mut h = 0;
    while h < state.holders.len() {
        let mut k = 0;
        while k < state.holders[h].holds.len() {
            if paths_share_prefix(&state.holders[h].holds[k].place, &place) {
                return Err(Error {
                    file: state.file.clone(),
                    message: format!(
                        "cannot move `{}` while it is borrowed",
                        place_to_string(&place)
                    ),
                    span,
                });
            }
            k += 1;
        }
        // Also scan per-slot field_holds (Phase D).
        let mut f = 0;
        while f < state.holders[h].field_holds.len() {
            let mut k = 0;
            while k < state.holders[h].field_holds[f].borrows.len() {
                if paths_share_prefix(
                    &state.holders[h].field_holds[f].borrows[k].place,
                    &place,
                ) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot move `{}` while it is borrowed",
                            place_to_string(&place)
                        ),
                        span,
                    });
                }
                k += 1;
            }
            f += 1;
        }
        h += 1;
    }
    state.moved.push(MovedPlace { place, status: MoveStatus::Moved });
    Ok(())
}

fn paths_share_prefix(a: &Vec<String>, b: &Vec<String>) -> bool {
    let m = if a.len() < b.len() { a.len() } else { b.len() };
    let mut i = 0;
    while i < m {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn place_to_string(p: &Vec<String>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < p.len() {
        if i > 0 {
            s.push('.');
        }
        s.push_str(&p[i]);
        i += 1;
    }
    s
}

fn clone_held_borrows(holds: &Vec<HeldBorrow>) -> Vec<HeldBorrow> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < holds.len() {
        out.push(HeldBorrow {
            place: holds[i].place.clone(),
            mutable: holds[i].mutable,
        });
        i += 1;
    }
    out
}
