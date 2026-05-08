// CFG-based borrow checker.
//
// `mod.rs` is the per-function driver: builds a CFG, runs three
// dataflow analyses, surfaces their errors as user-facing borrowck
// errors, and writes the move snapshot back onto FuncTable for
// codegen's drop-flag synthesis. The actual borrow-checking lives in
// the submodules:
//
// - `cfg`      — CFG data types (Place/Projection/Operand/BasicBlock/...)
// - `build`    — AST → CFG lowering
// - `moves`    — forward dataflow on per-place move state
// - `liveness` — backward dataflow on per-LocalId live ranges
// - `borrows`  — forward NLL active-borrow tracking + conflict checks

mod build;
mod cfg;
mod borrows;
mod liveness;
mod moves;
mod regions;

use crate::ast::{Function, Item, Module};
use crate::span::Error;
use crate::typeck::{
    FuncTable, MovedPlace, RType, StructTable, TraitTable, func_lookup, template_lookup,
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
            Item::TypeAlias(_) => {}
            Item::Const(_) => {}
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
                // For generic-trait impls (`impl Add<u32> for Foo`),
                // append the per-impl-row `__trait_impl_<idx>` suffix
                // to mirror the path scheme typeck/codegen use.
                if ib.trait_path.is_some() {
                    if let Some(idx) = crate::typeck::find_trait_impl_idx_by_span(
                        traits,
                        current_file,
                        &ib.span,
                    ) {
                        if !traits.impls[idx].trait_args.is_empty() {
                            method_prefix.push(format!("__trait_impl_{}", idx));
                        }
                    }
                }
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
                    let _ = &target_rt;
                    check_function(
                        &ib.methods[k],
                        &method_prefix,
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
    path_prefix: &Vec<String>,
    current_file: &str,
    structs: &StructTable,
    enums: &crate::typeck::EnumTable,
    traits: &TraitTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let mut full = path_prefix.clone();
    full.push(func.name.clone());

    // CFG-based borrow check. Build a control-flow graph for the
    // function, then run three dataflow passes:
    //   1. Move/init analysis (forward) — tracks per-place move state,
    //      reports use-after-move, partial-move-of-Drop, and produces
    //      the per-binding move snapshot codegen consumes for drop-flag
    //      synthesis.
    //   2. Liveness (backward) — per-local live ranges, used by NLL
    //      borrow pruning.
    //   3. Active-borrow analysis (forward) — tracks active references
    //      at each program point and reports borrow conflicts (mutable
    //      aliasing, write-blocked-by-borrow, move-blocked-by-borrow).
    let cfg_moved: Vec<MovedPlace>;
    let cfg_move_sites: Vec<(crate::ast::NodeId, String)>;
    {
        let funcs_ro: &FuncTable = &*funcs;
        let (param_types, expr_types, method_resolutions, call_resolutions, bare_closure_calls, type_params, type_param_bounds, pattern_ergo, lifetime_params, lifetime_predicates, const_uses) =
            if let Some(entry) = func_lookup(funcs_ro, &full) {
                (
                    &entry.param_types,
                    &entry.expr_types,
                    &entry.method_resolutions,
                    &entry.call_resolutions,
                    &entry.bare_closure_calls,
                    Vec::<String>::new(),
                    Vec::<Vec<Vec<String>>>::new(),
                    &entry.pattern_ergo,
                    &entry.lifetime_params,
                    &entry.lifetime_predicates,
                    &entry.const_uses,
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
                    &t.bare_closure_calls,
                    t.type_params.clone(),
                    bounds_clone,
                    &t.pattern_ergo,
                    &t.lifetime_params,
                    &t.lifetime_predicates,
                    &t.const_uses,
                )
            } else {
                unreachable!("typeck registered this function");
            };

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
        let cfg_ctx = build::CfgBuildCtx {
            structs,
            enums,
            traits,
            funcs: funcs_ro,
            expr_types,
            method_resolutions,
            call_resolutions,
            bare_closure_calls,
            const_uses,
            type_params: &type_params,
            type_param_bounds: &type_param_bounds,
            param_types,
            return_type: &return_ty,
            pattern_ergo,
            lifetime_params,
            lifetime_predicates,
        };
        let cfg = build::build(func, &cfg_ctx);
        // Phase L4: outlives solver. Validates each body-required
        // outlives constraint (FnReturn, CallArg, …) against the
        // closure of declared facts (WhereClause, StaticOutlives).
        regions::solve(&cfg.region_graph, &full, current_file)?;
        let move_analysis = moves::analyze(&cfg, traits, current_file);
        if let Some(e) = move_analysis.errors.into_iter().next() {
            return Err(e);
        }
        let liveness = liveness::analyze(&cfg);
        let borrow_check = borrows::analyze(&cfg, &liveness, current_file);
        if let Some(e) = borrow_check.errors.into_iter().next() {
            return Err(e);
        }
        // Re-run move analysis to recover its data (consumed-by-value
        // above when we drained errors). It's deterministic, so the
        // second run produces identical output.
        let move_analysis = moves::analyze(&cfg, traits, current_file);
        cfg_moved = build_moved_places(&cfg, &move_analysis);
        cfg_move_sites = move_analysis.move_sites;
    }

    let mut k = 0;
    while k < funcs.entries.len() {
        if funcs.entries[k].path == full {
            funcs.entries[k].moved_places = cfg_moved;
            funcs.entries[k].move_sites = cfg_move_sites;
            return Ok(());
        }
        k += 1;
    }
    let mut k = 0;
    while k < funcs.templates.len() {
        if funcs.templates[k].path == full {
            funcs.templates[k].moved_places = cfg_moved;
            funcs.templates[k].move_sites = cfg_move_sites;
            return Ok(());
        }
        k += 1;
    }
    unreachable!("typeck registered this function");
}

// Convert the CFG move analysis's `MovedLocal` entries (per-local
// move-state at scope end) into the `MovedPlace` shape codegen consumes
// for drop-flag synthesis. Each MovedLocal becomes a single-segment
// place keyed by the local's name.
fn build_moved_places(
    cfg: &cfg::Cfg,
    move_analysis: &moves::MoveAnalysis,
) -> Vec<MovedPlace> {
    let mut out: Vec<MovedPlace> = Vec::new();
    let mut i = 0;
    while i < move_analysis.moved_locals.len() {
        let m = &move_analysis.moved_locals[i];
        if let Some(name) = &cfg.locals[m.local as usize].name {
            let status = match m.status {
                moves::MoveStatus::Moved => crate::typeck::MoveStatus::Moved,
                moves::MoveStatus::MaybeMoved => {
                    crate::typeck::MoveStatus::MaybeMoved
                }
                // Uninit at scope-end means the binding was never
                // assigned (so its slot's bytes are garbage) — codegen
                // must skip Drop, same as Moved.
                moves::MoveStatus::Uninit => crate::typeck::MoveStatus::Moved,
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
