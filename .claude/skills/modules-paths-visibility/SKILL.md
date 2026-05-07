---
name: modules-paths-visibility
description: Use when working with `mod` declarations, file resolution across crates, `use` statements, the implicit prelude, `pub` visibility, `pub use` re-exports, or path resolution. Covers how single- and multi-segment paths resolve into struct/trait/func lookups across the crate tree.
---

# modules, paths, visibility

## Module declarations

`mod NAME;` at any module scope. The compiler resolves the child by trying two paths in order:
1. `<parent_dir>/<parent_stem>/<NAME>.rs` — nested. Used when the parent file has its own subdirectory of submodules, e.g. `lib/std/primitive.rs` with `lib/std/primitive/pointer.rs`.
2. `<parent_dir>/<NAME>.rs` — sibling. Flat layout, used by crate-root files like `lib.rs` and the existing top-level stdlib modules.

No inline `mod NAME { … }` syntax yet, no `super::` (use `crate::…` instead).

## Use statements

- `use a::b::c;` — single import.
- `use a::b::c as d;` — rename.
- `use a::*;` — glob.
- `use a::{b, c::*, d as e};` — brace multi, with arbitrary nesting, glob, and rename inside.
- `use a::b::{self, c};` — `self` inside a brace re-imports the prefix path itself alongside the named children. Optional rename (`{self as foo}`) overrides the local name. Parser encodes the leaf as `path: ["self"]` (sentinel); `flatten_use_tree` recognizes it and substitutes the brace's prefix as the imported absolute path.
- `use crate::…` — absolute-from-crate-root.

Allowed at module level (`Item::Use`) and inside any block (`Stmt::Use`); block-level uses scope to the enclosing block.

AST: `UseDecl { tree: UseTree }` where `UseTree` is `Leaf { path, rename }` / `Nested { prefix, children }` / `Glob { path }`.

`flatten_use_tree` produces a flat `Vec<UseEntry>` where each entry is `Explicit { local_name, full_path }` or `Glob { module_path }`. A leading `crate` segment is rewritten by `rewrite_crate_prefix` — for the user crate (name == "") it's stripped, for libraries it's substituted to the library name.

## Use-scope resolution

Walks the active scope (module-level + block-level entries, innermost-last) in reverse:
1. An explicit-import match on the path's first segment wins (the imported full path replaces just that first segment, the rest is appended).
2. Otherwise each glob's `module_path :: path` is tried against the relevant lookup table (struct / trait / func) and the first successful probe wins.

Path lookups in `resolve_type`, `resolve_trait_path`, `check_call`, and `check_struct_lit` all consult the use scope before falling back to module-relative lookup. Cast-target types are recorded per-NodeId by typeck so codegen reads them from `expr_types[expr.id]` rather than re-resolving (which avoids needing use-scope plumbing in codegen).

## Implicit prelude

`Library` carries a `prelude: bool` field. For each library where `prelude == true`, `compile` injects a synthetic `use <lib_name>::*;` at every other crate's root module before typeck — that is, at the user crate's root and at every other library's root (a library is never its own prelude, since it defines those items). The host (e.g. `main.rs`) opts `std` in; library users that don't want a prelude leave it `false`.

The injection is centralized in `inject_preludes(module, libraries, self_name)`. This is the canonical way to make stdlib items unqualified — no special-case fallback in path resolution.

## Visibility

`pub` modifier on functions, structs, struct fields, traits, impl methods (inherent — trait-impl methods inherit), `use` declarations. AST nodes carry `is_pub: bool`; typeck tables (`FnSymbol`/`GenericTemplate`/`StructEntry`/`StructField`/`TraitEntry`/`UseEntry`) propagate the flag.

The check `is_visible_from(defining_module, is_pub, accessor_module)` mirrors Rust: pub items go anywhere; non-pub items reach only the defining module and its descendants.

The defining module is the path's prefix excluding the item's own name (and excluding the impl-target name for methods, via `fn_defining_module(path, is_method=true)`).

Wired into:
- struct lookups (in `resolve_type`, `check_struct_lit`)
- trait lookups (in `resolve_trait_path`)
- function-call lookups (in `check_call`)
- per-field reads/writes (in `check_field_access` and the struct-literal field initializer check via `field_visible_from`)

Errors read like `function/struct/trait/field `X` is private`.

## `pub use` re-exports

A `pub use foo::Bar;` in module M makes `M::Bar` resolve (from outside M) to `foo::Bar`. `build_reexport_table` walks every module recursively at typeck-setup, collecting Explicit (single-item) re-exports into `ReExportTable.entries: Vec<ReExport { module, local_name, target }>`.

After a direct lookup misses in `resolve_type` / `resolve_trait_path` / `check_call` / `check_struct_lit`, `resolve_via_reexports(path, table, probe)` walks the table, finds entries whose `module + [local_name]` matches the path's `(prefix, last)`, and follows the target chain (depth-bounded at 16 hops to detect cycles).

Glob `pub use foo::*;` re-exports parse but aren't expanded at lookup time yet — explicit re-exports cover the bootstrap path. The implicit prelude glob (`use std::*;`) is non-pub: it makes std items reachable inside the user crate only, not re-exported to anyone consuming the user crate.

## Path resolution semantics

Every path in an expression is interpreted relative to the module containing the call. Single-segment identifiers without `(...)`/`{...}` are variable references; with `(...)` they're calls; with `{...}` they're struct literals. Multi-segment paths must be calls or struct literals. Only top-level (crate-root) functions are exported under their bare name.

The crate root's `name` drives its path prefix: a library is created with `name = "std"` so its items live at `["std", ...]`, while the user crate has `name = ""` so its items live at the empty prefix. The "export iff `current_module.is_empty()`" rule in codegen then naturally exports user crate-root functions and never library functions (libraries' top-level functions sit at `["std"]`, etc., so they're not exported even though they're emitted into the WASM module).

Errors in library code are attributed to the file paths the library's VFS was populated with (e.g. `lib.rs`, `dummy.rs`) — not synthetic `<std>/...` paths.
