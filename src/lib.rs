mod ast;
mod borrowck;
mod codegen;
mod layout;
mod lexer;
mod mono;
mod parser;
mod safeck;
mod span;
mod typeck;
pub mod wasm;

use ast::{Item, Module};
use span::{Error, Pos, Span};
use typeck::{EnumTable, FuncTable, StructTable, TraitTable};

pub struct File {
    pub path: String,
    pub source: String,
}

pub struct Vfs {
    pub files: Vec<File>,
}

impl Vfs {
    pub fn new() -> Vfs {
        Vfs { files: Vec::new() }
    }

    pub fn insert(&mut self, path: String, source: String) {
        let mut i = 0;
        while i < self.files.len() {
            if self.files[i].path == path {
                self.files[i].source = source;
                return;
            }
            i += 1;
        }
        self.files.push(File { path, source });
    }

    pub fn get(&self, path: &str) -> Option<&str> {
        let mut i = 0;
        while i < self.files.len() {
            if self.files[i].path == path {
                return Some(&self.files[i].source);
            }
            i += 1;
        }
        None
    }
}

pub struct Library {
    pub name: String,
    pub vfs: Vfs,
    pub entry: String,
    // When true, the host wants this library to act as a prelude for
    // the user crate — at the user crate's root module we inject a
    // synthetic `use <name>::*;` so the library's top-level items are
    // reachable unqualified. The host (e.g. `main.rs`) sets this to
    // `true` for `std`; tooling that doesn't want any prelude leaves
    // it `false`. Multiple libraries can opt in; each contributes its
    // own glob entry, with later-listed libraries shadowing earlier
    // ones (innermost-last in the use scope).
    pub prelude: bool,
}

pub fn compile(
    libraries: &[Library],
    user_vfs: &Vfs,
    user_entry: &str,
) -> Result<wasm::Module, String> {
    if user_vfs.get(user_entry).is_none() {
        return Err(format!("entry file not found in VFS: {}", user_entry));
    }
    let dummy = Pos::new(1, 1);

    let mut structs = StructTable {
        entries: Vec::new(),
    };
    let mut enums = EnumTable {
        entries: Vec::new(),
    };
    let mut traits = TraitTable {
        entries: Vec::new(),
        impls: Vec::new(),
    };
    let mut reexports = typeck::ReExportTable {
        entries: Vec::new(),
    };
    let mut funcs = FuncTable {
        entries: Vec::new(),
        templates: Vec::new(),
        inherent_synth_specs: Vec::new(),
    };
    let mut wasm_mod = wasm::Module::new();
    // Reserve a host-imported `env.panic(ptr: i32, len: i32)`
    // function at wasm function index 0. `panic!(msg)` lowers to a
    // call to this slot followed by `unreachable`. Hosts that
    // instantiate the module must provide the import (the test
    // harness registers a wasmi-trap stub; production hosts can
    // print and abort).
    wasm_mod.types.push(wasm::FuncType {
        params: vec![wasm::ValType::I32, wasm::ValType::I32],
        results: Vec::new(),
    });
    wasm_mod.imports.push(wasm::Import {
        module: "env".to_string(),
        name: "panic".to_string(),
        type_idx: 0,
    });
    // Module-defined functions are assigned wasm indices starting
    // after the imports — keep `next_idx` aligned with that.
    let mut next_idx: u32 = wasm_mod.imports.len() as u32;
    // One linear memory, fixed at 1 page (64 KiB). Stack pointer global lives
    // at index 0, initialized to the top of the page; shadow stack grows down.
    wasm_mod.memories.push(wasm::Memory {
        min_pages: 1,
        max_pages: Some(1),
    });
    // Export the memory as `"memory"` so hosts can read panic messages
    // (and inspect arbitrary wasm-side data). This matches the
    // standard wasm convention.
    wasm_mod.exports.push(wasm::Export {
        name: "memory".to_string(),
        kind: wasm::ExportKind::Memory,
        index: 0,
    });
    // Global 0: shadow-stack pointer (`__sp`). Initialized to the top
    // of the page (65536); shadow stack grows down for spilled
    // bindings, enum construction, sret slots, etc.
    wasm_mod.globals.push(wasm::Global {
        ty: wasm::ValType::I32,
        mutable: true,
        init: wasm::Instruction::I32Const(65536),
    });
    // Global 1: heap pointer (`__heap_top`). Bump-allocated by `¤alloc`,
    // grows upward from offset 8 (offset 0..7 reserved as null-pointer
    // territory for future debugging). `¤free` is currently a no-op
    // stub; allocations are leaked. Heap and shadow stack collide
    // silently if either grows past the other — there's no OOM check.
    wasm_mod.globals.push(wasm::Global {
        ty: wasm::ValType::I32,
        mutable: true,
        init: wasm::Instruction::I32Const(8),
    });

    let mut i = 0;
    while i < libraries.len() {
        let lib = &libraries[i];
        if lib.vfs.get(&lib.entry).is_none() {
            return Err(format!(
                "library `{}` entry file not found: {}",
                lib.name, lib.entry
            ));
        }
        let dummy_span = Span::new(dummy.copy(), dummy.copy());
        let mut lib_root = match resolve_module(&lib.vfs, &lib.entry, &lib.name, dummy_span) {
            Ok(m) => m,
            Err(e) => return Err(span::format_error(&e)),
        };
        // Each library gets the prelude injected at its root for every
        // OTHER prelude library — but never for itself, since a
        // library can't depend on its own prelude (it's defining the
        // prelude items). E.g. when compiling `core` alongside `std`,
        // `core` doesn't see `std::*` even if `std.prelude == true`.
        // Today there's only one prelude library (`std`), so this is
        // a no-op for std itself and applies to any future libraries.
        inject_preludes(&mut lib_root, libraries, Some(&lib.name));
        if let Err(e) = typeck::check(&lib_root, &mut structs, &mut enums, &mut traits, &mut funcs, &mut reexports, &mut next_idx) {
            return Err(span::format_error(&e));
        }
        if let Err(e) = borrowck::check(&lib_root, &structs, &enums, &traits, &mut funcs) {
            return Err(span::format_error(&e));
        }
        if let Err(e) = safeck::check(&lib_root, &funcs) {
            return Err(span::format_error(&e));
        }
        if let Err(e) = codegen::emit(&mut wasm_mod, &lib_root, &structs, &enums, &traits, &funcs, &mut next_idx) {
            return Err(span::format_error(&e));
        }
        i += 1;
    }

    let dummy_span = Span::new(dummy.copy(), dummy);
    let mut user_root = match resolve_module(user_vfs, user_entry, "", dummy_span) {
        Ok(m) => m,
        Err(e) => return Err(span::format_error(&e)),
    };
    // User crate gets every prelude library — there's no "self" to
    // exclude.
    inject_preludes(&mut user_root, libraries, None);
    if let Err(e) = typeck::check(&user_root, &mut structs, &mut enums, &mut traits, &mut funcs, &mut reexports, &mut next_idx) {
        return Err(span::format_error(&e));
    }
    if let Err(e) = borrowck::check(&user_root, &structs, &enums, &traits, &mut funcs) {
        return Err(span::format_error(&e));
    }
    if let Err(e) = safeck::check(&user_root, &funcs) {
        return Err(span::format_error(&e));
    }
    if let Err(e) = codegen::emit(&mut wasm_mod, &user_root, &structs, &enums, &traits, &funcs, &mut next_idx) {
        return Err(span::format_error(&e));
    }
    Ok(wasm_mod)
}

// Inject `use <lib>::*;` at `module`'s root for every library in
// `libraries` whose `prelude` flag is set, except the library named
// `self_name` (if any) — a library can't be its own prelude since it
// defines the items the prelude imports. The injected entries are
// non-pub: the prelude makes names unqualified inside `module` only,
// not re-exported.
fn inject_preludes(module: &mut Module, libraries: &[Library], self_name: Option<&str>) {
    let mut i = 0;
    while i < libraries.len() {
        let lib = &libraries[i];
        if lib.prelude && !lib.name.is_empty() {
            let is_self = matches!(self_name, Some(n) if n == lib.name);
            if !is_self {
                let prelude_span = Span::new(Pos::new(1, 1), Pos::new(1, 1));
                let mut path: Vec<String> = Vec::new();
                path.push(lib.name.clone());
                module.items.insert(
                    0,
                    ast::Item::Use(ast::UseDecl {
                        tree: ast::UseTree::Glob {
                            path,
                            span: prelude_span.copy(),
                        },
                        is_pub: false,
                        span: prelude_span,
                    }),
                );
            }
        }
        i += 1;
    }
}

fn resolve_module(
    vfs: &Vfs,
    file_path: &str,
    mod_name: &str,
    name_span: Span,
) -> Result<Module, Error> {
    let source = vfs.get(file_path).expect("file existence checked by caller");
    let tokens = lexer::tokenize(file_path, source)?;
    let raw_items = parser::parse(file_path, tokens)?;
    let mut items: Vec<Item> = Vec::new();
    for raw in raw_items {
        match raw {
            parser::RawItem::Function(f) => items.push(Item::Function(f)),
            parser::RawItem::Struct(sd) => items.push(Item::Struct(sd)),
            parser::RawItem::Enum(ed) => items.push(Item::Enum(ed)),
            parser::RawItem::Impl(ib) => items.push(Item::Impl(ib)),
            parser::RawItem::Trait(td) => items.push(Item::Trait(td)),
            parser::RawItem::Use(u) => items.push(Item::Use(u)),
            parser::RawItem::ModDecl {
                name: child_name,
                name_span: child_name_span,
            } => {
                let (nested, sibling) = compute_child_paths(file_path, &child_name);
                // Prefer the nested path (`<parent_stem>/<child>.rs`) when
                // it exists. Falls back to the sibling path
                // (`<child>.rs`) — the original behaviour.
                let child_path = if vfs.get(&nested).is_some() {
                    nested
                } else if vfs.get(&sibling).is_some() {
                    sibling
                } else {
                    return Err(Error {
                        file: file_path.to_string(),
                        message: format!(
                            "module file not found: tried `{}` and `{}`",
                            nested, sibling
                        ),
                        span: child_name_span,
                    });
                };
                let child = resolve_module(vfs, &child_path, &child_name, child_name_span)?;
                items.push(Item::Module(child));
            }
        }
    }
    Ok(Module {
        name: mod_name.to_string(),
        name_span,
        source_file: file_path.to_string(),
        items,
    })
}

// Two candidate paths for a `mod child;` declared inside `parent_path`:
//
// - `nested`:  `<dir>/<parent_stem>/<child>.rs` — the convention when
//              the parent file has its own subdirectory of submodules
//              (e.g. `lib/std/primitive.rs` + `lib/std/primitive/pointer.rs`).
// - `sibling`: `<dir>/<child>.rs` — the convention when submodules sit
//              flat next to their parent (e.g. `lib/std/lib.rs` +
//              `lib/std/marker.rs`). Also used by crate-root files
//              (`lib.rs`, `main.rs`).
//
// `resolve_module` prefers `nested` when it exists, otherwise tries
// `sibling`. Both are returned so the error message at the call site
// can name both candidates.
fn compute_child_paths(parent_path: &str, child_name: &str) -> (String, String) {
    let (dir, parent_stem) = match parent_path.rfind('/') {
        Some(idx) => {
            let dir = &parent_path[..idx];
            let file = &parent_path[idx + 1..];
            let stem = file.strip_suffix(".rs").unwrap_or(file);
            (dir.to_string(), stem.to_string())
        }
        None => {
            let stem = parent_path.strip_suffix(".rs").unwrap_or(parent_path);
            ("".to_string(), stem.to_string())
        }
    };
    let nested = if dir.is_empty() {
        format!("{}/{}.rs", parent_stem, child_name)
    } else {
        format!("{}/{}/{}.rs", dir, parent_stem, child_name)
    };
    let sibling = if dir.is_empty() {
        format!("{}.rs", child_name)
    } else {
        format!("{}/{}.rs", dir, child_name)
    };
    (nested, sibling)
}
