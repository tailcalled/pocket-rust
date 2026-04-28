mod ast;
mod borrowck;
mod codegen;
mod lexer;
mod parser;
mod span;
mod typeck;
pub mod wasm;

use ast::{Item, Module};
use span::{Error, Pos, Span};
use typeck::{FuncTable, StructTable};

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
    let mut funcs = FuncTable {
        entries: Vec::new(),
    };
    let mut next_idx: u32 = 0;
    let mut wasm_mod = wasm::Module::new();

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
        let lib_root = match resolve_module(&lib.vfs, &lib.entry, &lib.name, dummy_span) {
            Ok(m) => m,
            Err(e) => return Err(span::format_error(&e)),
        };
        if let Err(e) = typeck::check(&lib_root, &mut structs, &mut funcs, &mut next_idx) {
            return Err(span::format_error(&e));
        }
        if let Err(e) = borrowck::check(&lib_root, &structs, &funcs) {
            return Err(span::format_error(&e));
        }
        if let Err(e) = codegen::emit(&mut wasm_mod, &lib_root, &structs, &funcs) {
            return Err(span::format_error(&e));
        }
        i += 1;
    }

    let dummy_span = Span::new(dummy.copy(), dummy);
    let user_root = match resolve_module(user_vfs, user_entry, "", dummy_span) {
        Ok(m) => m,
        Err(e) => return Err(span::format_error(&e)),
    };
    if let Err(e) = typeck::check(&user_root, &mut structs, &mut funcs, &mut next_idx) {
        return Err(span::format_error(&e));
    }
    if let Err(e) = borrowck::check(&user_root, &structs, &funcs) {
        return Err(span::format_error(&e));
    }
    if let Err(e) = codegen::emit(&mut wasm_mod, &user_root, &structs, &funcs) {
        return Err(span::format_error(&e));
    }
    Ok(wasm_mod)
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
            parser::RawItem::ModDecl {
                name: child_name,
                name_span: child_name_span,
            } => {
                let child_path = compute_child_path(file_path, &child_name);
                if vfs.get(&child_path).is_none() {
                    return Err(Error {
                        file: file_path.to_string(),
                        message: format!("module file not found: `{}`", child_path),
                        span: child_name_span,
                    });
                }
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

fn compute_child_path(parent_path: &str, child_name: &str) -> String {
    match parent_path.rfind('/') {
        Some(idx) => format!("{}/{}.rs", &parent_path[..idx], child_name),
        None => format!("{}.rs", child_name),
    }
}
