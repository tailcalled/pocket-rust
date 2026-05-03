// `gaps` test inventory.
//
// Naming: each test name describes the BEHAVIOR pocket-rust SHOULD
// have (e.g. `..._is_rejected`, `..._returns_42`, `..._roundtrips`),
// not the current broken state. The test asserts the correct behavior
// directly — it fails today because pocket-rust doesn't yet implement
// it. When the gap is fixed, the test starts passing and gets
// promoted to its proper home (`tests/lang/`, `tests/std/`, etc.).
//
// Tests are NOT `#[ignore]`'d and NOT inverted. CI reports them as
// honest failures. The count of failing tests in this suite is the
// outstanding-gap budget.

use pocket_rust::{Library, Vfs, compile};
use std::fs;
use std::path::Path;

pub fn load_stdlib() -> Library {
    let stdlib_path = Path::new("lib/std");
    let mut vfs = Vfs::new();
    load_dir(stdlib_path, stdlib_path, &mut vfs);
    Library {
        name: "std".to_string(),
        vfs,
        entry: "lib.rs".to_string(),
        prelude: true,
    }
}

fn load_dir(root: &Path, dir: &Path, vfs: &mut Vfs) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file_type");
        if file_type.is_dir() {
            load_dir(root, &path, vfs);
        } else if file_type.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rs")
        {
            let rel = path.strip_prefix(root).expect("strip_prefix");
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            vfs.insert(key, fs::read_to_string(&path).expect("read_to_string"));
        }
    }
}

// Expects compile failure — used for "pocket-rust should reject this
// but currently accepts it" gaps.
pub fn compile_source(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
}

// Expects compile success — used for "pocket-rust should accept this
// but currently rejects it" gaps. Panics with the compiler's error
// message if compile fails.
pub fn compile_inline(source: &str) -> Vec<u8> {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    let module = compile(&libs, &vfs, "lib.rs")
        .unwrap_or_else(|e| panic!("expected compile success, got: {}", e));
    module.encode()
}

mod borrowck;
mod typeck;
