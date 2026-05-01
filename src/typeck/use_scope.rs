use super::{
    FuncTable, StructEntry, StructTable, TraitEntry, TraitTable, funcs_entry_index, struct_lookup, template_lookup, trait_lookup,
};

// Re-export entry: a `pub use foo::Bar;` in module M makes the name
// `M::Bar` (or `M::<rename>` for `pub use foo::Bar as Q;`) resolve
// to `foo::Bar`. The table lets cross-module path lookups follow
// these re-exports — without it, outside callers would have to know
// the original definition's path even when the re-export is the
// public API.
#[derive(Clone)]
pub struct ReExport {
    pub module: Vec<String>,
    pub local_name: String,
    pub target: Vec<String>,
}

pub struct ReExportTable {
    pub entries: Vec<ReExport>,
}

// Walk every module recursively, collecting every `pub use ...`
// entry. Each `pub use foo::Bar;` (or renamed) in module M produces a
// ReExport entry. Globs `pub use foo::*;` register a wildcard re-
// export that's expanded lazily at lookup time.
pub fn build_reexport_table(root: &crate::ast::Module) -> ReExportTable {
    let mut table = ReExportTable { entries: Vec::new() };
    let mut path: Vec<String> = Vec::new();
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
    let crate_root: String = if path.is_empty() {
        String::new()
    } else {
        path[0].clone()
    };
    collect_reexports_in_module(root, &mut path, &crate_root, &mut table);
    table
}

fn collect_reexports_in_module(
    module: &crate::ast::Module,
    path: &mut Vec<String>,
    crate_root: &str,
    table: &mut ReExportTable,
) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            crate::ast::Item::Use(u) if u.is_pub => {
                // Flatten this pub use's tree into UseEntries (with the
                // crate-root rewrite), then turn each Explicit entry
                // into a ReExport at the current module.
                let mut entries: Vec<UseEntry> = Vec::new();
                flatten_use_tree(&Vec::new(), &u.tree, crate_root, true, &mut entries);
                let mut k = 0;
                while k < entries.len() {
                    if let UseEntry::Explicit { local_name, full_path, .. } = &entries[k] {
                        table.entries.push(ReExport {
                            module: path.clone(),
                            local_name: local_name.clone(),
                            target: full_path.clone(),
                        });
                    }
                    // Globs: a `pub use foo::*;` would need lazy
                    // expansion at lookup time — skip for now (not in
                    // the bootstrap path). Documented as a limitation.
                    k += 1;
                }
            }
            crate::ast::Item::Module(m) => {
                path.push(m.name.clone());
                collect_reexports_in_module(m, path, crate_root, table);
                path.pop();
            }
            _ => {}
        }
        i += 1;
    }
}

// Apply re-exports to a path lookup. If `path` is `[mod..., name]`
// and `[mod...]` has a `pub use ... as name;`, return the target. May
// chain through multiple levels (a re-export of a re-export). Caller
// passes `probe` to validate the final destination resolves in their
// table; we stop chaining once probe accepts.
pub fn resolve_via_reexports<F>(
    path: &Vec<String>,
    table: &ReExportTable,
    probe: F,
) -> Option<Vec<String>>
where
    F: Fn(&Vec<String>) -> bool,
{
    if path.is_empty() {
        return None;
    }
    let mut current = path.clone();
    let mut depth = 0;
    while depth < 16 {
        if probe(&current) {
            return Some(current);
        }
        // Try every "split point" where current[..split] could match a
        // re-export's `module + local_name` (so split runs from
        // current.len() down to 1). The longest split wins, which
        // mirrors the natural `[a, b, c]` interpretation: prefer
        // matching the full prefix `a::b::c` as a re-export over just
        // `a::b`. Once we find a match, substitute the target into
        // current and continue chasing chains.
        //
        // This covers the middle-segment case: for `std::Vec::new`,
        // the split = 2 attempt finds re-export `std::Vec ->
        // std::vec::Vec`, producing `std::vec::Vec::new`.
        let mut found: Option<Vec<String>> = None;
        let mut split = current.len();
        while split >= 1 {
            let module_len = split - 1;
            let local_name = &current[module_len];
            let mut i = 0;
            while i < table.entries.len() {
                let e = &table.entries[i];
                if e.module.len() == module_len && &e.local_name == local_name {
                    let mut module_eq = true;
                    let mut k = 0;
                    while k < module_len {
                        if e.module[k] != current[k] {
                            module_eq = false;
                            break;
                        }
                        k += 1;
                    }
                    if module_eq {
                        // Substitute: target + remaining tail.
                        let mut new_path = e.target.clone();
                        let mut k = split;
                        while k < current.len() {
                            new_path.push(current[k].clone());
                            k += 1;
                        }
                        found = Some(new_path);
                        break;
                    }
                }
                i += 1;
            }
            if found.is_some() {
                break;
            }
            split -= 1;
        }
        match found {
            Some(t) => {
                current = t;
                depth += 1;
            }
            None => return None,
        }
    }
    None
}

// Re-export-aware lookups. When the user writes a path that matches
// a `pub use` re-export, the actual table holds the entry under the
// canonical (re-export target) path — these helpers transparently
// follow the re-export chain so callers don't have to.
pub fn trait_lookup_resolved<'a>(
    traits: &'a TraitTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<&'a TraitEntry> {
    if let Some(e) = trait_lookup(traits, path) {
        return Some(e);
    }
    let target = resolve_via_reexports(path, reexports, |p| {
        trait_lookup(traits, p).is_some()
    })?;
    trait_lookup(traits, &target)
}

pub fn struct_lookup_resolved<'a>(
    structs: &'a StructTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<&'a StructEntry> {
    if let Some(e) = struct_lookup(structs, path) {
        return Some(e);
    }
    let target = resolve_via_reexports(path, reexports, |p| {
        struct_lookup(structs, p).is_some()
    })?;
    struct_lookup(structs, &target)
}

pub fn func_path_resolved(
    funcs: &FuncTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<Vec<String>> {
    if funcs_entry_index(funcs, path).is_some() || template_lookup(funcs, path).is_some() {
        return Some(path.clone());
    }
    resolve_via_reexports(path, reexports, |p| {
        funcs_entry_index(funcs, p).is_some() || template_lookup(funcs, p).is_some()
    })
}

// Visibility check: an item with `is_pub` flag, defined inside
// `defining_module`, is visible from `accessor_module` iff `is_pub`
// or `accessor_module` is `defining_module` or a descendant. Mirrors
// Rust's "private items are visible to the defining module and its
// descendants."

// A flattened entry from a `use` declaration. `Explicit` corresponds
// to `use a::b::c;` (or a renamed `use a::b::c as d;`) — single name
// → single full path. `Glob` corresponds to `use a::b::*;` — every
// item directly under `a::b` is brought into scope, resolved lazily
// at lookup time via probing the relevant table.
//
// `is_pub` carries the originating `UseDecl.is_pub` — for `pub use`,
// the entry contributes to the enclosing module's re-export table
// (see `ReExportTable`) so outside modules can reach the imported
// item via `<this_module>::<local_name>`.
#[derive(Clone)]
pub enum UseEntry {
    Explicit {
        local_name: String,
        full_path: Vec<String>,
        is_pub: bool,
    },
    Glob {
        module_path: Vec<String>,
        is_pub: bool,
    },
}

// Recursively flatten a UseTree into a list of UseEntry, with `prefix`
// prepended to every contained path. Top-level callers pass an empty
// prefix; the recursion accumulates prefix segments through Nested.
//
// A leading `crate` segment in any use path is rewritten to the
// enclosing crate's root: for the user crate (root_name == "") it's
// stripped (so `use crate::foo::bar;` becomes `["foo","bar"]`); for a
// library (e.g. root_name == "std") it's substituted (so `use
// crate::Drop` inside std's own source becomes `["std","Drop"]`).
// The prefix is applied first, then the crate-rewrite acts on the
// resulting absolute path.
pub fn flatten_use_tree(
    prefix: &Vec<String>,
    tree: &crate::ast::UseTree,
    crate_root: &str,
    is_pub: bool,
    out: &mut Vec<UseEntry>,
) {
    match tree {
        crate::ast::UseTree::Leaf { path, rename, .. } => {
            let mut full = prefix.clone();
            let mut i = 0;
            while i < path.len() {
                full.push(path[i].clone());
                i += 1;
            }
            // Local name comes from the *original* last segment (or
            // explicit rename) — `use crate::foo::Bar;` imports `Bar`,
            // not `crate`, even after the rewrite below.
            let local_name = match rename {
                Some(r) => r.clone(),
                None => {
                    if full.is_empty() {
                        return; // nothing to import
                    }
                    full[full.len() - 1].clone()
                }
            };
            full = rewrite_crate_prefix(full, crate_root);
            out.push(UseEntry::Explicit {
                local_name,
                full_path: full,
                is_pub,
            });
        }
        crate::ast::UseTree::Glob { path, .. } => {
            let mut full = prefix.clone();
            let mut i = 0;
            while i < path.len() {
                full.push(path[i].clone());
                i += 1;
            }
            full = rewrite_crate_prefix(full, crate_root);
            out.push(UseEntry::Glob {
                module_path: full,
                is_pub,
            });
        }
        crate::ast::UseTree::Nested { prefix: p, children, .. } => {
            let mut combined = prefix.clone();
            let mut i = 0;
            while i < p.len() {
                combined.push(p[i].clone());
                i += 1;
            }
            let mut k = 0;
            while k < children.len() {
                flatten_use_tree(&combined, &children[k], crate_root, is_pub, out);
                k += 1;
            }
        }
    }
}

pub fn rewrite_crate_prefix(mut path: Vec<String>, crate_root: &str) -> Vec<String> {
    if !path.is_empty() && path[0] == "crate" {
        if crate_root.is_empty() {
            // User crate: drop the `crate` segment entirely. Items
            // live at the empty-prefix root, so `crate::foo::bar`
            // becomes just `foo::bar`.
            let mut rest: Vec<String> = Vec::new();
            let mut i = 1;
            while i < path.len() {
                rest.push(path[i].clone());
                i += 1;
            }
            return rest;
        } else {
            // Library: substitute `crate` → library name. So inside
            // `std`'s source, `use crate::Drop;` becomes `std::Drop`.
            path[0] = crate_root.to_string();
            return path;
        }
    }
    path
}

// Apply use-table resolution to a path. Looks at the path's first
// segment; if it matches an explicit use, the imported full path
// replaces just that first segment (the rest of the path is appended).
// If no explicit match, each glob in scope is tried by prefixing the
// glob's module path to the original path and probing the resulting
// candidate against the caller's lookup target. Returns the
// use-resolved path, or `None` if no use entry applied.
//
// `scope` is a single flat list of `UseEntry`s, ordered with
// outermost-first / innermost-last; iteration is reverse so the
// innermost scope's entries shadow outer ones.
//
// Examples (with `use std::Drop;` and `use std::*;`):
//   - `Drop` → `std::Drop` (explicit match, single segment).
//   - `Pair::new` (with `use foo::Pair;`) → `foo::Pair::new` (the
//     imported `Pair` becomes the path root; the rest follows).
//   - `Drop` (with only `use std::*;`, no explicit) → `std::Drop`
//     iff probe(["std","Drop"]) succeeds.
pub fn resolve_via_use_scopes<F>(
    path: &[String],
    scope: &Vec<UseEntry>,
    probe: F,
) -> Option<Vec<String>>
where
    F: Fn(&Vec<String>) -> bool,
{
    if path.is_empty() {
        return None;
    }
    let head = &path[0];
    // Explicit match on the first segment — innermost (last-pushed) wins.
    let mut s = scope.len();
    while s > 0 {
        s -= 1;
        if let UseEntry::Explicit { local_name, full_path, .. } = &scope[s] {
            if local_name == head {
                let mut out = full_path.clone();
                let mut j = 1;
                while j < path.len() {
                    out.push(path[j].clone());
                    j += 1;
                }
                return Some(out);
            }
        }
    }
    // No explicit; try each glob's `module_path :: path` in reverse.
    let mut s = scope.len();
    while s > 0 {
        s -= 1;
        if let UseEntry::Glob { module_path, .. } = &scope[s] {
            let mut candidate = module_path.clone();
            let mut j = 0;
            while j < path.len() {
                candidate.push(path[j].clone());
                j += 1;
            }
            if probe(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

// Walk a Module's items and flatten every `use` declaration into a
// single `Vec<UseEntry>`. `crate_root` is the enclosing crate's name
// (empty for the user crate, or e.g. `"std"` for a library), used by
// `flatten_use_tree` to rewrite leading `crate` segments. Submodule
// uses don't propagate up.
pub fn module_use_entries(module: &crate::ast::Module, crate_root: &str) -> Vec<UseEntry> {
    let mut out: Vec<UseEntry> = Vec::new();
    let mut i = 0;
    while i < module.items.len() {
        if let crate::ast::Item::Use(u) = &module.items[i] {
            flatten_use_tree(&Vec::new(), &u.tree, crate_root, u.is_pub, &mut out);
        }
        i += 1;
    }
    out
}
// Visibility checks for items within and across modules. The basic rule:
// a `pub` item is visible from anywhere; a non-pub item is visible only
// from its defining module and that module's descendants.
//
// Callers pass `defining_module` explicitly so the rule applies
// uniformly to free functions, structs, traits, and methods —
// methods nest under their impl target's name in the path, but the
// defining module is still the enclosing module, not the struct.
pub fn is_visible_from(
    defining_module: &Vec<String>,
    is_pub: bool,
    accessor_module: &Vec<String>,
) -> bool {
    if is_pub {
        return true;
    }
    if accessor_module.len() < defining_module.len() {
        return false;
    }
    let mut i = 0;
    while i < defining_module.len() {
        if accessor_module[i] != defining_module[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Defining module for a function-table path: free functions live at
// `[mod..., name]` (drop one), inherent/trait-impl methods live at
// `[mod..., StructName, method_name]` (drop two). The
// `is_method_path` flag is computed from `FnSymbol.impl_target`.
pub fn fn_defining_module(item_path: &Vec<String>, is_method: bool) -> Vec<String> {
    let drop = if is_method { 2 } else { 1 };
    let n = if item_path.len() >= drop {
        item_path.len() - drop
    } else {
        0
    };
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < n {
        out.push(item_path[i].clone());
        i += 1;
    }
    out
}

// Defining module for a struct/trait at `[mod..., name]`.
pub fn type_defining_module(item_path: &Vec<String>) -> Vec<String> {
    if item_path.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < item_path.len() {
        out.push(item_path[i].clone());
        i += 1;
    }
    out
}

// Field-level visibility: a non-pub struct field is only accessible
// from inside the struct's defining module (or any descendant).
pub fn field_visible_from(
    struct_path: &Vec<String>,
    field_is_pub: bool,
    accessor_module: &Vec<String>,
) -> bool {
    is_visible_from(
        &type_defining_module(struct_path),
        field_is_pub,
        accessor_module,
    )
}
